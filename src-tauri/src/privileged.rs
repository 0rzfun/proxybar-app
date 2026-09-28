//! One authenticated, app-lifetime connection to a root worker. No listening root service.
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::{
        io::AsRawFd,
        net::{UnixListener, UnixStream},
    },
    process::{Command, Stdio},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

type Connection = BufReader<UnixStream>;
static SESSION: OnceLock<Result<Mutex<Connection>, String>> = OnceLock::new();

#[derive(Serialize, Deserialize)]
struct Request {
    command: String,
    cleanup: Option<String>,
    pid_file: Option<String>,
    clear: bool,
}

fn peer_pid(stream: &UnixStream) -> Result<u32> {
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of_val(&pid) as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&mut pid as *mut libc::pid_t).cast(),
            &mut len,
        )
    };
    anyhow::ensure!(
        result == 0 && pid > 0,
        "cannot authenticate administrator connection"
    );
    Ok(pid as u32)
}

fn peer_is_root(stream: &UnixStream) -> bool {
    let mut uid = 1;
    let mut gid = 0;
    unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) == 0 && uid == 0 }
}

pub async fn clear_cleanup() -> Result<()> {
    if !matches!(SESSION.get(), Some(Ok(_))) {
        return Ok(());
    }
    send(Request {
        command: String::new(),
        cleanup: None,
        pid_file: None,
        clear: true,
    })
    .await
}

pub fn shutdown() {
    if let Some(Ok(session)) = SESSION.get() {
        if let Ok(connection) = session.lock() {
            let _ = connection.get_ref().shutdown(std::net::Shutdown::Both);
        }
    }
}

pub fn initialize() -> Result<()> {
    SESSION
        .get_or_init(|| connect().map(Mutex::new).map_err(|e| e.to_string()))
        .as_ref()
        .map(|_| ())
        .map_err(|e| anyhow!(e.clone()))
}

fn connect() -> Result<Connection> {
    // mkdtemp creates the directory atomically with mode 0700.
    let mut template = b"/tmp/proxybar-helper-XXXXXX\0".to_vec();
    let directory = unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) };
    anyhow::ensure!(
        !directory.is_null(),
        "cannot create administrator socket directory"
    );
    let directory = std::path::PathBuf::from(
        unsafe { std::ffi::CStr::from_ptr(directory) }
            .to_string_lossy()
            .into_owned(),
    );
    let result = (|| {
        let socket = directory.join("control");
        let listener = UnixListener::bind(&socket)?;
        listener.set_nonblocking(true)?;
        let executable = std::env::current_exe()?;
        let command = format!(
            "{} --proxybar-privileged {} {} </dev/null >/dev/null 2>&1 & echo $!",
            crate::proxy::shell_quote(&executable.to_string_lossy()),
            crate::proxy::shell_quote(&socket.to_string_lossy()),
            std::process::id(),
        );
        let script = format!(
            "do shell script \"{}\" with administrator privileges",
            command.replace('\\', "\\\\").replace('"', "\\\""),
        );
        let output = Command::new("/usr/bin/osascript")
            .args(["-e", &script])
            .output()?;
        anyhow::ensure!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        let expected: u32 = String::from_utf8_lossy(&output.stdout).trim().parse()?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match listener.accept() {
                Ok((stream, _)) if peer_pid(&stream)? == expected && peer_is_root(&stream) => {
                    return blocking_connection(stream);
                }
                Ok(_) if Instant::now() < deadline => continue,
                Ok(_) => return Err(anyhow!("administrator connection timed out")),
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(e) => return Err(e.into()),
            }
        }
    })();
    let _ = std::fs::remove_file(directory.join("control"));
    let _ = std::fs::remove_dir(directory);
    result
}

fn blocking_connection(stream: UnixStream) -> Result<Connection> {
    // macOS accept() inherits O_NONBLOCK from the listening socket. Requests run
    // on spawn_blocking and must wait for the worker instead of failing with EAGAIN.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(45)))?;
    stream.set_write_timeout(Some(Duration::from_secs(45)))?;
    Ok(BufReader::new(stream))
}

pub async fn execute(
    command: &str,
    cleanup: Option<String>,
    pid_file: Option<String>,
) -> Result<()> {
    send(Request {
        command: command.to_owned(),
        cleanup,
        pid_file,
        clear: false,
    })
    .await
}

async fn send(request: Request) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        initialize()?;
        let session = SESSION
            .get()
            .unwrap()
            .as_ref()
            .map_err(|e| anyhow!(e.clone()))?;
        let mut connection = session
            .lock()
            .map_err(|_| anyhow!("administrator connection poisoned"))?;
        let result = exchange(&mut connection, &request);
        // Never reuse a stream after a timeout or partial reply: replies would be misaligned.
        if result.is_err() {
            let _ = connection.get_ref().shutdown(std::net::Shutdown::Both);
        }
        if let Some(error) = result? {
            return Err(anyhow!(error));
        }
        Ok(())
    })
    .await?
}

fn exchange(connection: &mut Connection, request: &Request) -> Result<Option<String>> {
    serde_json::to_writer(connection.get_mut(), request)?;
    connection.get_mut().write_all(b"\n")?;
    let mut response = String::new();
    anyhow::ensure!(
        connection.read_line(&mut response)? > 0,
        "administrator helper disconnected; restart ProxyBar"
    );
    Ok(serde_json::from_str(&response)?)
}

pub fn run_if_requested() -> bool {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).map(String::as_str) != Some("--proxybar-privileged") {
        return false;
    }
    if worker(&args).is_err() {
        std::process::exit(1);
    }
    true
}

fn worker(args: &[String]) -> Result<()> {
    anyhow::ensure!(
        args.len() == 4 && unsafe { libc::geteuid() } == 0,
        "invalid helper invocation"
    );
    let stream = UnixStream::connect(&args[2])?;
    anyhow::ensure!(
        peer_pid(&stream)? == args[3].parse::<u32>()?,
        "unexpected parent process"
    );
    serve(stream)
}

fn process_cleanup(pid: u32, identity: &str) -> String {
    let identity = crate::proxy::shell_quote(identity.trim_end());
    format!(
        "matches() {{ [ \"$(/bin/ps -p {pid} -o lstart=,command=)\" = {identity} ]; }}; if matches; then /bin/kill -TERM {pid}; i=0; while matches && [ $i -lt 30 ]; do /bin/sleep 0.1; i=$((i+1)); done; if matches; then /bin/kill -KILL {pid}; fi; fi"
    )
}

fn serve(stream: UnixStream) -> Result<()> {
    let mut connection = BufReader::new(stream);
    let mut cleanups = Vec::new();
    let result = (|| -> Result<()> {
        loop {
            let mut line = String::new();
            if connection.read_line(&mut line)? == 0 {
                break;
            }
            let request: Request = serde_json::from_str(&line)?;
            if request.clear {
                cleanups.clear();
            }
            // Register restoration before attempting the DNS mutation, including partial failures.
            if let Some(cleanup) = request.cleanup {
                cleanups.push(cleanup);
            }
            let output = Command::new("/bin/sh")
                .args(["-c", &request.command])
                .stdin(Stdio::null())
                .output()?;
            if let Some(path) = request.pid_file {
                let pid: u32 = std::fs::read_to_string(path)?.trim().parse()?;
                // Capture identity now; never read a mutable PID file during disconnect cleanup.
                let identity = Command::new("/bin/ps")
                    .args(["-p", &pid.to_string(), "-o", "lstart=,command="])
                    .output()?;
                if identity.status.success() && !identity.stdout.is_empty() {
                    cleanups.push(process_cleanup(pid, &String::from_utf8(identity.stdout)?));
                }
            }
            let error = if output.status.success() {
                None
            } else {
                Some(format!(
                    "administrator operation failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ))
            };
            serde_json::to_writer(connection.get_mut(), &error)?;
            connection.get_mut().write_all(b"\n")?;
        }
        Ok(())
    })();
    // EOF also covers crashes/forced termination of the UI process. Restore DNS before TUN.
    for cleanup in cleanups.iter().rev() {
        let _ = Command::new("/bin/sh")
            .args(["-c", cleanup])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    result.context("administrator worker failed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_connection_waits_for_delayed_worker_reply() {
        let directory =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.build/helper-tests");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("socket-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let peer = UnixStream::connect(&path).unwrap();
        let (accepted, _) = listener.accept().unwrap();
        let worker = std::thread::spawn(move || {
            let mut peer = BufReader::new(peer);
            let mut request = String::new();
            peer.read_line(&mut request).unwrap();
            std::thread::sleep(Duration::from_millis(50));
            peer.get_mut().write_all(b"null\n").unwrap();
        });
        let mut connection = blocking_connection(accepted).unwrap();
        assert_eq!(
            exchange(
                &mut connection,
                &Request {
                    command: "true".into(),
                    cleanup: None,
                    pid_file: None,
                    clear: false,
                }
            )
            .unwrap(),
            None
        );
        worker.join().unwrap();
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn reuses_one_connection_after_command_failure() {
        let (client, server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || serve(server));
        let mut client = BufReader::new(client);
        for (command, fails) in [
            ("true", false),
            ("echo failure >&2; exit 1", true),
            ("true", false),
        ] {
            let error = exchange(
                &mut client,
                &Request {
                    command: command.into(),
                    cleanup: None,
                    pid_file: None,
                    clear: false,
                },
            )
            .unwrap();
            assert_eq!(error.is_some(), fails);
        }
        drop(client);
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn cleanup_checks_identity_and_gracefully_stops_exact_pid() {
        let command = process_cleanup(
            123,
            "Mon Sep 28 12:00:00 2026 /Applications/Proxy Bar's/sing-box",
        );
        assert!(command.contains("/bin/ps -p 123 -o lstart=,command="));
        assert!(command.contains("Bar'\\''s"));
        assert!(command.contains("/bin/kill -TERM 123"));
        assert!(command.contains("if matches; then /bin/kill -KILL 123"));
    }

    #[test]
    fn disconnect_restores_dns_before_stopping_process_and_forgets_old_cleanup() {
        let directory =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.build/helper-tests");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("cleanup-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let quoted = crate::proxy::shell_quote(&path.to_string_lossy());
        let (client, server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || serve(server));
        let mut client = BufReader::new(client);
        for (cleanup, clear) in [
            (Some(format!("echo stale >> {quoted}")), false),
            (None, true),
            (Some(format!("echo process >> {quoted}")), false),
            (Some(format!("echo dns >> {quoted}")), false),
        ] {
            assert!(exchange(
                &mut client,
                &Request {
                    command: "true".into(),
                    cleanup,
                    pid_file: None,
                    clear,
                },
            )
            .unwrap()
            .is_none());
        }
        drop(client);
        worker.join().unwrap().unwrap();
        let result = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(result, "dns\nprocess\n");
    }

    #[test]
    fn disconnected_worker_is_not_treated_as_success() {
        let (client, server) = UnixStream::pair().unwrap();
        drop(server);
        let mut client = BufReader::new(client);
        assert!(exchange(
            &mut client,
            &Request {
                command: "true".into(),
                cleanup: None,
                pid_file: None,
                clear: false,
            },
        )
        .is_err());
    }
}
