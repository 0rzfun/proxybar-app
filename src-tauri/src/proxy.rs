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
    let config = serde_json::to_vec_pretty(&crate::sing_box::config(
        node,
        port,
        mode,
        &paths.sing_box_cache,
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
    configure_managed_process(&mut command);
    let child = command
        .spawn()
        .with_context(|| format!("failed to start {}", paths.sing_box.display()))?;
    hide_managed_process_console(&child);
    Ok(ManagedProcess::Child(child))
}

async fn wait_until_ready(
    paths: &AppPaths,
    mut process: ManagedProcess,
    port: u16,
) -> Result<ManagedProcess> {
    for _ in 0..200 {
        let running = match &mut process {
            ManagedProcess::Child(child) => child.try_wait()?.is_none(),
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
        Some(ManagedProcess::Child(mut child)) => {
            #[cfg(windows)]
            if let Some(pid) = child.id() {
                send_windows_interrupt(pid);
            }
            #[cfg(not(windows))]
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
        #[cfg(target_os = "macos")]
        Some(ManagedProcess::Elevated { pid }) => stop_elevated(paths, pid).await,
        None => stop_stale_elevated(paths).await,
    }
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
fn configure_managed_process(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command
        .as_std_mut()
        .creation_flags(CREATE_NEW_CONSOLE | CREATE_NEW_PROCESS_GROUP);
}

#[cfg(not(windows))]
fn configure_managed_process(_: &mut Command) {}

#[cfg(windows)]
fn hide_managed_process_console(child: &tokio::process::Child) {
    use windows_sys::Win32::{
        System::Console::{AttachConsole, FreeConsole, GetConsoleWindow},
        UI::WindowsAndMessaging::{ShowWindow, SW_HIDE},
    };

    let Some(pid) = child.id() else { return };
    unsafe {
        // Release builds use the Windows GUI subsystem and therefore have no
        // console of their own. Keep an attached development console intact.
        if GetConsoleWindow().is_null() && AttachConsole(pid) != 0 {
            let window = GetConsoleWindow();
            if !window.is_null() {
                let _ = ShowWindow(window, SW_HIDE);
            }
            let _ = FreeConsole();
        }
    }
}

#[cfg(not(windows))]
fn hide_managed_process_console(_: &tokio::process::Child) {}

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
    use super::elevated_launch_command;

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
