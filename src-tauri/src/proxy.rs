#[cfg(windows)]
use crate::model::WindowsProcess;
use crate::model::{AppPaths, ManagedProcess, Node, ProxyMode};
use anyhow::{anyhow, Context, Result};
use std::{fs::OpenOptions, process::Stdio, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    process::Command,
};

pub async fn available_port(port: u16) -> Result<u16> {
    if TcpListener::bind(("127.0.0.1", port)).await.is_ok() {
        Ok(port)
    } else {
        Err(anyhow!("local proxy port {port} is already in use"))
    }
}

pub async fn start_sing_box(
    paths: &AppPaths,
    node: &Node,
    port: u16,
    mode: ProxyMode,
) -> Result<ManagedProcess> {
    let system_dns_servers = system_dns_servers(mode).await;
    let config = serde_json::to_vec_pretty(&crate::sing_box::config(
        node,
        port,
        mode,
        &paths.sing_box_cache,
        &system_dns_servers,
    ))?;
    tokio::fs::write(&paths.sing_box_config, config).await?;
    validate_config(paths).await?;
    let _ = tokio::fs::remove_file(&paths.sing_box_log).await;

    #[cfg(target_os = "macos")]
    let process = if mode.uses_tun() {
        start_elevated(paths).await?
    } else {
        start_child(paths)?
    };
    #[cfg(not(target_os = "macos"))]
    let process = start_child(paths)?;

    wait_until_ready(paths, process, port).await
}

#[cfg(target_os = "macos")]
async fn system_dns_servers(mode: ProxyMode) -> Vec<String> {
    if !mode.uses_tun() {
        return Vec::new();
    }
    let Ok(output) = Command::new("/usr/sbin/scutil")
        .arg("--dns")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    parse_macos_dns_servers(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(target_os = "macos")]
fn parse_macos_dns_servers(output: &str) -> Vec<String> {
    use std::{collections::HashSet, net::IpAddr};

    let mut seen = HashSet::new();
    output
        .lines()
        .filter_map(|line| line.trim().strip_prefix("nameserver["))
        .filter_map(|line| line.split_once(" : ").map(|(_, address)| address.trim()))
        .filter_map(|address| address.split('%').next())
        .filter_map(|address| address.parse::<IpAddr>().ok())
        .filter(|address| match address {
            IpAddr::V4(address) => {
                !address.is_unspecified() && !address.is_loopback() && !address.is_multicast()
            }
            IpAddr::V6(address) => {
                !address.is_unspecified()
                    && !address.is_loopback()
                    && !address.is_multicast()
                    && !address.is_unicast_link_local()
            }
        })
        .map(|address| address.to_string())
        .filter(|address| seen.insert(address.clone()))
        .collect()
}

#[cfg(not(target_os = "macos"))]
async fn system_dns_servers(_mode: ProxyMode) -> Vec<String> {
    Vec::new()
}

async fn validate_config(paths: &AppPaths) -> Result<()> {
    let mut command = Command::new(&paths.sing_box);
    command
        .arg("check")
        .arg("-c")
        .arg(&paths.sing_box_config)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_console(&mut command);
    let output = command.output().await?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let message = if stderr.is_empty() { stdout } else { stderr };
    Err(anyhow!("sing-box configuration is invalid: {message}"))
}

#[cfg(not(windows))]
fn start_child(paths: &AppPaths) -> Result<ManagedProcess> {
    let log = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&paths.sing_box_log)?;
    let stdout = log.try_clone()?;
    let mut command = Command::new(&paths.sing_box);
    command
        .arg("run")
        .arg("-c")
        .arg(&paths.sing_box_config)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(log));
    Ok(ManagedProcess::Child(command.spawn().with_context(
        || format!("failed to start {}", paths.sing_box.display()),
    )?))
}

#[cfg(windows)]
fn start_child(paths: &AppPaths) -> Result<ManagedProcess> {
    use std::{ffi::OsString, os::windows::ffi::OsStrExt, os::windows::io::AsRawHandle};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT},
        System::Threading::{
            CreateProcessW, CREATE_NEW_CONSOLE, CREATE_NEW_PROCESS_GROUP, PROCESS_INFORMATION,
            STARTF_USESHOWWINDOW, STARTF_USESTDHANDLES, STARTUPINFOW,
        },
        UI::WindowsAndMessaging::SW_HIDE,
    };

    let log = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&paths.sing_box_log)?;
    let stdin = OpenOptions::new().read(true).open("NUL")?;
    let log_handle = log.as_raw_handle() as HANDLE;
    let stdin_handle = stdin.as_raw_handle() as HANDLE;

    for handle in [stdin_handle, log_handle] {
        let changed =
            unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
        if changed == 0 {
            return Err(std::io::Error::last_os_error())
                .context("failed to inherit sing-box log handles");
        }
    }

    let mut application: Vec<u16> = paths
        .sing_box
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let mut command = OsString::new();
    command.push("\"");
    command.push(&paths.sing_box);
    command.push("\" run -c \"");
    command.push(&paths.sing_box_config);
    command.push("\"");
    let mut command: Vec<u16> = command.encode_wide().chain(Some(0)).collect();
    let mut startup = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        dwFlags: STARTF_USESHOWWINDOW | STARTF_USESTDHANDLES,
        wShowWindow: SW_HIDE as u16,
        hStdInput: stdin_handle,
        hStdOutput: log_handle,
        hStdError: log_handle,
        ..Default::default()
    };
    let mut process = PROCESS_INFORMATION::default();
    let created = unsafe {
        CreateProcessW(
            application.as_mut_ptr(),
            command.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            CREATE_NEW_CONSOLE | CREATE_NEW_PROCESS_GROUP,
            std::ptr::null(),
            std::ptr::null(),
            &mut startup,
            &mut process,
        )
    };
    for handle in [stdin_handle, log_handle] {
        unsafe {
            let _ = SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
        }
    }
    if created == 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to start {}", paths.sing_box.display()));
    }
    unsafe {
        let _ = CloseHandle(process.hThread);
    }
    Ok(ManagedProcess::Windows(WindowsProcess {
        pid: process.dwProcessId,
        handle: process.hProcess,
    }))
}

async fn wait_until_ready(
    paths: &AppPaths,
    mut process: ManagedProcess,
    port: u16,
) -> Result<ManagedProcess> {
    for _ in 0..200 {
        let running = match &mut process {
            #[cfg(not(windows))]
            ManagedProcess::Child(child) => child.try_wait()?.is_none(),
            #[cfg(windows)]
            ManagedProcess::Windows(process) => !windows_process_exited(process)?,
            #[cfg(target_os = "macos")]
            ManagedProcess::Elevated { pid } => elevated_process_matches(paths, *pid).await,
        };
        if !running {
            let message = process_error(paths).await;
            return Err(anyhow!("sing-box exited during startup: {message}"));
        }
        if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            return Ok(process);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let message = process_error(paths).await;
    let _ = stop_sing_box(paths, Some(process)).await;
    Err(anyhow!("sing-box did not become ready: {message}"))
}

async fn process_error(paths: &AppPaths) -> String {
    match tokio::fs::read_to_string(&paths.sing_box_log).await {
        Ok(log) if !log.trim().is_empty() => log
            .lines()
            .rev()
            .take(8)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n"),
        _ => "no diagnostic output was produced".into(),
    }
}

pub async fn stop_sing_box(paths: &AppPaths, process: Option<ManagedProcess>) -> Result<()> {
    match process {
        #[cfg(not(windows))]
        Some(ManagedProcess::Child(mut child)) => {
            let _ = child.start_kill();

            if tokio::time::timeout(Duration::from_secs(3), child.wait())
                .await
                .is_err()
            {
                let _ = child.start_kill();
                let _ = child.wait().await;
            }
            Ok(())
        }
        #[cfg(windows)]
        Some(ManagedProcess::Windows(process)) => {
            send_windows_interrupt(process.pid);
            for _ in 0..30 {
                if windows_process_exited(&process)? {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            windows_terminate_process(&process)?;
            for _ in 0..30 {
                if windows_process_exited(&process)? {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(anyhow!("sing-box did not exit after termination"))
        }
        #[cfg(target_os = "macos")]
        Some(ManagedProcess::Elevated { pid }) => stop_elevated(paths, pid).await,
        None => stop_stale_elevated(paths).await,
    }
}

pub fn process_exited(process: &mut ManagedProcess) -> Result<bool> {
    match process {
        #[cfg(not(windows))]
        ManagedProcess::Child(child) => Ok(child.try_wait()?.is_some()),
        #[cfg(windows)]
        ManagedProcess::Windows(process) => windows_process_exited(process),
        #[cfg(target_os = "macos")]
        ManagedProcess::Elevated { .. } => Ok(false),
    }
}

pub async fn socks_port_ready(port: u16) -> bool {
    tokio::time::timeout(
        Duration::from_millis(500),
        TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    .is_ok_and(|result| result.is_ok())
}

#[cfg(target_os = "macos")]
async fn start_elevated(paths: &AppPaths) -> Result<ManagedProcess> {
    let _ = tokio::fs::remove_file(&paths.sing_box_pid).await;
    let command = elevated_launch_command(
        &paths.sing_box.to_string_lossy(),
        &paths.sing_box_config.to_string_lossy(),
        &paths.sing_box_log.to_string_lossy(),
        &paths.sing_box_pid.to_string_lossy(),
    );
    run_as_administrator(&command).await?;

    for _ in 0..30 {
        if let Ok(contents) = tokio::fs::read_to_string(&paths.sing_box_pid).await {
            if let Ok(pid) = contents.trim().parse::<u32>() {
                return Ok(ManagedProcess::Elevated { pid });
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(anyhow!(
        "sing-box administrator process did not provide a PID"
    ))
}

#[cfg(target_os = "macos")]
async fn stop_stale_elevated(paths: &AppPaths) -> Result<()> {
    let Ok(contents) = tokio::fs::read_to_string(&paths.sing_box_pid).await else {
        return Ok(());
    };
    let Ok(pid) = contents.trim().parse::<u32>() else {
        let _ = tokio::fs::remove_file(&paths.sing_box_pid).await;
        return Ok(());
    };
    stop_elevated(paths, pid).await
}

#[cfg(not(target_os = "macos"))]
async fn stop_stale_elevated(_paths: &AppPaths) -> Result<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
async fn stop_elevated(paths: &AppPaths, pid: u32) -> Result<()> {
    if elevated_process_matches(paths, pid).await {
        let command = format!(
            "/bin/kill -TERM {pid}; i=0; while /bin/kill -0 {pid} 2>/dev/null && [ $i -lt 30 ]; do /bin/sleep 0.1; i=$((i+1)); done; if /bin/kill -0 {pid} 2>/dev/null; then /bin/kill -KILL {pid}; fi"
        );
        run_as_administrator(&command).await?;
    }
    let _ = tokio::fs::remove_file(&paths.sing_box_pid).await;
    Ok(())
}

#[cfg(target_os = "macos")]
async fn elevated_process_matches(paths: &AppPaths, pid: u32) -> bool {
    let output = Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .await;
    let Ok(output) = output else { return false };
    if !output.status.success() {
        return false;
    }
    let command = String::from_utf8_lossy(&output.stdout);
    command.contains(&*paths.sing_box.to_string_lossy())
        && command.contains(&*paths.sing_box_config.to_string_lossy())
}

#[cfg(target_os = "macos")]
async fn run_as_administrator(command: &str) -> Result<()> {
    let escaped = command.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!("do shell script \"{escaped}\" with administrator privileges");
    let output = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await?;
    if output.status.success() {
        Ok(())
    } else {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(anyhow!(if message.is_empty() {
            "administrator permission was denied".into()
        } else {
            message
        }))
    }
}

#[cfg(target_os = "macos")]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(target_os = "macos")]
fn elevated_launch_command(binary: &str, config: &str, log: &str, pid: &str) -> String {
    format!(
        "{} run -c {} < /dev/null > {} 2>&1 & /bin/echo $! > {}",
        shell_quote(binary),
        shell_quote(config),
        shell_quote(log),
        shell_quote(pid)
    )
}

#[cfg(windows)]
fn hide_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.as_std_mut().creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console(_: &mut Command) {}

#[cfg(windows)]
fn windows_process_exited(process: &WindowsProcess) -> Result<bool> {
    use windows_sys::Win32::{
        Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::Threading::WaitForSingleObject,
    };
    match unsafe { WaitForSingleObject(process.handle, 0) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        _ => Err(std::io::Error::last_os_error()).context("failed to inspect sing-box process"),
    }
}

#[cfg(windows)]
fn windows_terminate_process(process: &WindowsProcess) -> Result<()> {
    use windows_sys::Win32::System::Threading::TerminateProcess;
    if unsafe { TerminateProcess(process.handle, 1) } == 0 {
        return Err(std::io::Error::last_os_error()).context("failed to terminate sing-box");
    }
    Ok(())
}

#[cfg(windows)]
fn send_windows_interrupt(pid: u32) {
    use windows_sys::Win32::System::Console::{
        AttachConsole, FreeConsole, GenerateConsoleCtrlEvent, SetConsoleCtrlHandler,
        CTRL_BREAK_EVENT,
    };

    unsafe {
        let _ = FreeConsole();
        if AttachConsole(pid) != 0 {
            let _ = SetConsoleCtrlHandler(None, 1);
            let _ = GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid);
            let _ = FreeConsole();
            let _ = SetConsoleCtrlHandler(None, 0);
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::{elevated_launch_command, parse_macos_dns_servers};

    #[test]
    fn extracts_unique_routable_macos_dns_servers() {
        let output = r#"
resolver #1
  nameserver[0] : 192.168.1.111
  nameserver[1] : fd00::1
resolver #2
  nameserver[0] : 192.168.1.111
  nameserver[1] : fe80::1%en0
  nameserver[2] : 127.0.0.1
  nameserver[3] : invalid
"#;
        assert_eq!(
            parse_macos_dns_servers(output),
            vec!["192.168.1.111", "fd00::1"]
        );
    }

    #[test]
    fn elevated_launch_detaches_without_macos_nohup() {
        let command = elevated_launch_command(
            "/Applications/Proxy Bar/sing-box",
            "/tmp/config.json",
            "/tmp/sing-box.log",
            "/tmp/sing-box.pid",
        );
        assert!(!command.contains("nohup"));
        assert!(command.contains("< /dev/null"));
        assert!(command.contains("'/Applications/Proxy Bar/sing-box'"));
        assert!(command.ends_with("/bin/echo $! > '/tmp/sing-box.pid'"));
    }
}
