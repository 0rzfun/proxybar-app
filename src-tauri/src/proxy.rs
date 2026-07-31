#[cfg(target_os = "macos")]
use crate::model::MacosDnsOverride;
#[cfg(windows)]
use crate::model::WindowsProcess;
use crate::model::{AppPaths, ManagedProcess, Node, ProxyMode};
use anyhow::{anyhow, Context, Result};
use std::{fs::OpenOptions, process::Stdio, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    process::Command,
};

#[cfg(target_os = "macos")]
const TUN_DNS_GATEWAY: &str = "172.19.0.2";

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
    #[cfg(target_os = "macos")]
    let dns_override = if mode.uses_tun() {
        Some(capture_macos_dns_override().await?.0)
    } else {
        None
    };
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

    let mut process = wait_until_ready(paths, process, port).await?;

    #[cfg(target_os = "macos")]
    if let Some(dns_override) = dns_override {
        if let Err(error) = apply_macos_dns_override(&dns_override).await {
            let _ = stop_sing_box(paths, Some(process)).await;
            return Err(error);
        }
        if let ManagedProcess::Elevated {
            dns_override: process_override,
            ..
        } = &mut process
        {
            *process_override = Some(dns_override);
        }
    }

    Ok(process)
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
        .filter(|address| address.to_string() != TUN_DNS_GATEWAY)
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

#[cfg(target_os = "macos")]
async fn capture_macos_dns_override() -> Result<(MacosDnsOverride, bool)> {
    let route = Command::new("/sbin/route")
        .args(["-n", "get", "default"])
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("failed to inspect the macOS default route")?;
    if !route.status.success() {
        return Err(anyhow!("macOS default route is unavailable"));
    }
    let interface = parse_macos_default_interface(&String::from_utf8_lossy(&route.stdout))
        .ok_or_else(|| anyhow!("macOS default route has no interface"))?;

    let services = Command::new("/usr/sbin/networksetup")
        .arg("-listnetworkserviceorder")
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("failed to list macOS network services")?;
    if !services.status.success() {
        return Err(anyhow!("macOS network services are unavailable"));
    }
    let service =
        parse_macos_network_service(&String::from_utf8_lossy(&services.stdout), &interface)
            .ok_or_else(|| anyhow!("no macOS network service uses interface {interface}"))?;

    let configured = Command::new("/usr/sbin/networksetup")
        .args(["-getdnsservers", &service])
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .with_context(|| format!("failed to read DNS servers for {service}"))?;
    if !configured.status.success() {
        return Err(anyhow!("failed to read DNS servers for {service}"));
    }
    let configured = String::from_utf8_lossy(&configured.stdout);
    let had_tun_gateway = configured
        .lines()
        .any(|line| line.trim() == TUN_DNS_GATEWAY);
    Ok((
        MacosDnsOverride {
            service,
            original_servers: parse_macos_configured_dns(&configured),
        },
        had_tun_gateway,
    ))
}

#[cfg(target_os = "macos")]
fn parse_macos_default_interface(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        line.trim()
            .strip_prefix("interface:")
            .map(str::trim)
            .filter(|interface| !interface.is_empty())
            .map(str::to_owned)
    })
}

#[cfg(target_os = "macos")]
fn parse_macos_network_service(output: &str, interface: &str) -> Option<String> {
    let mut service = None;
    for line in output.lines().map(str::trim) {
        if line.starts_with('(') {
            if let Some((_, name)) = line.split_once(") ") {
                service = Some(name.trim_start_matches('*').trim().to_owned());
                continue;
            }
        }
        if line.contains(&format!("Device: {interface})")) {
            return service.filter(|service| !service.is_empty());
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn parse_macos_configured_dns(output: &str) -> Vec<String> {
    use std::net::IpAddr;

    output
        .lines()
        .map(str::trim)
        .filter(|line| line.parse::<IpAddr>().is_ok())
        .filter(|line| *line != TUN_DNS_GATEWAY)
        .map(str::to_owned)
        .collect()
}

#[cfg(target_os = "macos")]
async fn apply_macos_dns_override(dns_override: &MacosDnsOverride) -> Result<()> {
    let command = format!(
        "/usr/sbin/networksetup -setdnsservers {} {} && {{ /usr/bin/dscacheutil -flushcache; /usr/bin/killall -HUP mDNSResponder 2>/dev/null || true; }}",
        shell_quote(&dns_override.service),
        shell_quote(TUN_DNS_GATEWAY),
    );
    run_as_administrator(&command)
        .await
        .with_context(|| format!("failed to route macOS DNS through {}", dns_override.service))
}

#[cfg(target_os = "macos")]
fn restore_macos_dns_command(dns_override: &MacosDnsOverride) -> String {
    let servers = if dns_override.original_servers.is_empty() {
        "empty".to_owned()
    } else {
        dns_override
            .original_servers
            .iter()
            .map(|server| shell_quote(server))
            .collect::<Vec<_>>()
            .join(" ")
    };
    format!(
        "/usr/sbin/networksetup -setdnsservers {} {servers} && {{ /usr/bin/dscacheutil -flushcache; /usr/bin/killall -HUP mDNSResponder 2>/dev/null || true; }}",
        shell_quote(&dns_override.service),
    )
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
            ManagedProcess::Elevated { pid, .. } => elevated_process_matches(paths, *pid).await,
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
        Some(ManagedProcess::Elevated { pid, dns_override }) => {
            stop_elevated(paths, pid, dns_override.as_ref()).await
        }
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
                return Ok(ManagedProcess::Elevated {
                    pid,
                    dns_override: None,
                });
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
    let stale_dns = match capture_macos_dns_override().await {
        Ok((dns_override, true)) => Some(dns_override),
        _ => None,
    };
    let pid = tokio::fs::read_to_string(&paths.sing_box_pid)
        .await
        .ok()
        .and_then(|contents| contents.trim().parse::<u32>().ok());
    match pid {
        Some(pid) => stop_elevated(paths, pid, stale_dns.as_ref()).await,
        None => {
            let _ = tokio::fs::remove_file(&paths.sing_box_pid).await;
            if let Some(dns_override) = stale_dns {
                run_as_administrator(&restore_macos_dns_command(&dns_override)).await?;
            }
            Ok(())
        }
    }
}

#[cfg(not(target_os = "macos"))]
async fn stop_stale_elevated(_paths: &AppPaths) -> Result<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
async fn stop_elevated(
    paths: &AppPaths,
    pid: u32,
    dns_override: Option<&MacosDnsOverride>,
) -> Result<()> {
    let process_matches = elevated_process_matches(paths, pid).await;
    let mut command = if let Some(dns_override) = dns_override {
        format!(
            "{}; restore_status=$?",
            restore_macos_dns_command(dns_override)
        )
    } else {
        "restore_status=0".to_owned()
    };
    if process_matches {
        command.push_str(&format!(
            "; /bin/kill -TERM {pid}; i=0; while /bin/kill -0 {pid} 2>/dev/null && [ $i -lt 30 ]; do /bin/sleep 0.1; i=$((i+1)); done; if /bin/kill -0 {pid} 2>/dev/null; then /bin/kill -KILL {pid}; fi"
        ));
    }
    command.push_str("; exit $restore_status");
    let result = if dns_override.is_some() || process_matches {
        run_as_administrator(&command).await
    } else {
        Ok(())
    };
    let _ = tokio::fs::remove_file(&paths.sing_box_pid).await;
    result
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
    use super::{
        elevated_launch_command, parse_macos_configured_dns, parse_macos_default_interface,
        parse_macos_dns_servers, parse_macos_network_service, restore_macos_dns_command,
    };
    use crate::model::MacosDnsOverride;

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
    fn finds_active_macos_network_service() {
        let route = "   route to: default\ninterface: en7\n";
        let services = r#"
An asterisk (*) denotes that a network service is disabled.
(1) Wi-Fi
(Hardware Port: Wi-Fi, Device: en0)
(2) Renamed USB LAN
(Hardware Port: USB 10/100/1000 LAN, Device: en7)
"#;
        assert_eq!(parse_macos_default_interface(route).as_deref(), Some("en7"));
        assert_eq!(
            parse_macos_network_service(services, "en7").as_deref(),
            Some("Renamed USB LAN")
        );
    }

    #[test]
    fn stale_tun_gateway_is_not_captured_as_original_dns() {
        assert_eq!(
            parse_macos_configured_dns("172.19.0.2\n1.1.1.1\n"),
            vec!["1.1.1.1"]
        );
    }

    #[test]
    fn restores_dhcp_dns_before_tun_shutdown() {
        let command = restore_macos_dns_command(&MacosDnsOverride {
            service: "Home Wi-Fi".into(),
            original_servers: Vec::new(),
        });
        assert!(command.starts_with("/usr/sbin/networksetup -setdnsservers 'Home Wi-Fi' empty"));
        assert!(command.contains("dscacheutil -flushcache"));
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
