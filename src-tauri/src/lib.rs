mod localization;
mod model;
#[cfg(target_os = "macos")]
mod privileged;
mod proxy;
mod sing_box;
mod subscription;

use anyhow::{anyhow, Context, Result};
use localization::Texts;
use model::{sing_box_binary_name, AppPaths, ProxyMode, Runtime, Settings};
use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex as StdMutex,
    },
    time::Duration,
};
use tauri::{
    image::Image,
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, State,
};
use tokio::sync::Mutex;

const TRAY_ID: &str = "proxybar-tray";
type ModeMenuItems = Vec<(ProxyMode, CheckMenuItem<tauri::Wry>)>;

pub struct AppState {
    paths: AppPaths,
    texts: Texts,
    inner: Mutex<Runtime>,
    transition: Mutex<()>,
    mode_items: StdMutex<ModeMenuItems>,
    quitting: AtomicBool,
}

#[derive(Serialize)]
struct SettingsState {
    subscription_url: String,
    proxy_port: u16,
    title: String,
    description: String,
    subscription_label: String,
    subscription_placeholder: String,
    proxy_port_label: String,
    confirm: String,
    cancel: String,
}

#[tauri::command]
async fn get_settings_state(state: State<'_, AppState>) -> Result<SettingsState, String> {
    let runtime = state.inner.lock().await;
    Ok(SettingsState {
        subscription_url: runtime.settings.subscription_url.clone(),
        proxy_port: runtime.settings.proxy_port,
        title: state.texts.get("settings_title").into(),
        description: state.texts.get("settings_description").into(),
        subscription_label: state.texts.get("subscription_label").into(),
        subscription_placeholder: state.texts.get("subscription_placeholder").into(),
        proxy_port_label: state.texts.get("proxy_port_label").into(),
        confirm: state.texts.get("confirm").into(),
        cancel: state.texts.get("cancel").into(),
    })
}

#[tauri::command]
fn hide_settings(app: AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("settings")
        .ok_or_else(|| "设置窗口不可用".to_string())?;
    window.hide().map_err(error_text)
}

#[tauri::command]
async fn save_settings(
    app: AppHandle,
    subscription_url: String,
    proxy_port: u32,
) -> Result<(), String> {
    let subscription_url = validate_optional_http_url(&subscription_url, "订阅地址")?;
    let proxy_port = u16::try_from(proxy_port)
        .ok()
        .filter(|port| *port >= 1024)
        .ok_or_else(|| "端口号必须在 1024 到 65535 之间".to_string())?;
    let state = app.state::<AppState>();
    let current_port = state.inner.lock().await.settings.proxy_port;
    if current_port != proxy_port {
        proxy::available_port(proxy_port)
            .await
            .map_err(|_| format!("端口 {proxy_port} 已被占用"))?;
    }
    let (settings, mode, subscription_changed, subscription_missing, port_changed) = {
        let mut runtime = state.inner.lock().await;
        let subscription_changed = runtime.settings.subscription_url != subscription_url;
        let port_changed = runtime.settings.proxy_port != proxy_port;
        runtime.settings.subscription_url = subscription_url;
        runtime.settings.proxy_port = proxy_port;
        runtime.settings.normalize_mode();
        if runtime.settings.mode == ProxyMode::Off {
            runtime.port = proxy_port;
        }
        (
            runtime.settings.clone(),
            runtime.settings.mode,
            subscription_changed,
            !runtime.settings.has_subscription(),
            port_changed,
        )
    };
    persist_settings(&state.paths, &settings)
        .await
        .map_err(error_text)?;
    if subscription_missing {
        rebuild_tray(&app).await.map_err(error_text)?;
        if let Err(error) = switch_mode(&app, ProxyMode::Off).await {
            recover_off(&app).await;
            return Err(error_text(error));
        }
    } else if subscription_changed {
        rebuild_tray(&app).await.map_err(error_text)?;
    }
    if let Some(window) = app.get_webview_window("settings") {
        let _ = window.hide();
    }
    if subscription_changed && !subscription_missing {
        spawn_refresh_subscription(app.clone());
    }
    if port_changed {
        let handle = app.clone();
        tauri::async_runtime::spawn(async move {
            if port_changed && mode != ProxyMode::Off {
                if let Err(error) = switch_mode(&handle, mode).await {
                    recover_off(&handle).await;
                    set_status(&handle, format!("{error:#}")).await;
                    return;
                }
            }
        });
    }
    Ok(())
}

fn validate_optional_http_url(value: &str, label: &str) -> Result<String, String> {
    if value.trim().is_empty() {
        return Ok(String::new());
    }
    validate_http_url(value, label)
}

fn validate_http_url(value: &str, label: &str) -> Result<String, String> {
    let parsed = url::Url::parse(value.trim()).map_err(|_| format!("{label}格式不正确"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!("{label}必须以 http:// 或 https:// 开头"));
    }
    Ok(parsed.to_string())
}

fn error_text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

pub fn run() {
    #[cfg(target_os = "macos")]
    {
        if privileged::run_if_requested() {
            return;
        }
        // Remember denial for this session; switching/restart tasks must not re-prompt.
        if let Err(error) = privileged::initialize() {
            eprintln!("{error}");
        }
    }
    let app = tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            get_settings_state,
            hide_settings,
            save_settings
        ])
        .on_window_event(|window, event| {
            if window.label() == "settings" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let paths = resolve_paths(app.handle())?;
            fs::create_dir_all(&paths.data_dir)?;
            ensure_executable(&paths.sing_box)?;
            let texts = Texts::load(&paths.locales);
            let settings = load_settings(&paths);
            let has_subscription = settings.has_subscription();
            let nodes = fs::read_to_string(&paths.subscription_cache)
                .map(|body| subscription::parse(&body))
                .unwrap_or_default();
            let runtime = Runtime {
                port: settings.proxy_port,
                settings,
                nodes,
                process: None,
                subscription_loading: false,
                status: String::new(),
                restart_pending: false,
                restart_attempts: 0,
            };

            let mode_items = create_tray(app.handle(), &runtime, &texts, &paths)?;
            let startup_mode = runtime.settings.mode;
            let has_nodes = !runtime.nodes.is_empty();
            app.manage(AppState {
                paths,
                texts,
                inner: Mutex::new(runtime),
                transition: Mutex::new(()),
                mode_items: StdMutex::new(mode_items),
                quitting: AtomicBool::new(false),
            });

            spawn_tray_theme_watcher(app.handle().clone());
            spawn_proxy_watcher(app.handle().clone());
            if has_subscription {
                spawn_refresh_subscription(app.handle().clone());
            } else {
                show_settings_window(app.handle());
            }
            if startup_mode != ProxyMode::Off && has_nodes {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(error) = switch_mode(&handle, startup_mode).await {
                        recover_off(&handle).await;
                        set_status(&handle, format!("{error:#}")).await;
                    }
                });
            } else {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(error) = switch_mode(&handle, ProxyMode::Off).await {
                        set_status(&handle, format!("{error:#}")).await;
                    }
                });
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build ProxyBar");

    app.run(|app, event| {
        if let tauri::RunEvent::ExitRequested { api, .. } = event {
            let state = app.state::<AppState>();
            if !state.quitting.load(Ordering::SeqCst) {
                api.prevent_exit();
                let handle = app.clone();
                tauri::async_runtime::spawn(async move { quit(&handle).await });
            }
        }
    });
}

fn create_tray(
    app: &AppHandle,
    runtime: &Runtime,
    texts: &Texts,
    paths: &AppPaths,
) -> Result<ModeMenuItems> {
    let (menu, mode_items) = tray_menu(app, runtime, texts)?;
    let icon = tray_icon(paths, runtime.settings.mode)?;
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .icon_as_template(cfg!(target_os = "macos"))
        .tooltip(tooltip(runtime, texts))
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| handle_menu(app.clone(), event.id().as_ref().to_owned()))
        .build(app)?;
    Ok(mode_items)
}

fn handle_menu(app: AppHandle, id: String) {
    if let Some(mode) = id.strip_prefix("mode.").and_then(parse_mode) {
        let checkmarks = set_mode_checkmarks(&app, mode);
        tauri::async_runtime::spawn(async move {
            if let Err(error) = checkmarks {
                set_status(&app, format!("{error:#}")).await;
            }
            if let Err(error) = switch_mode(&app, mode).await {
                recover_off(&app).await;
                set_status(&app, format!("{error:#}")).await;
            }
        });
    } else if let Some(index) = id
        .strip_prefix("node.")
        .and_then(|value| value.parse().ok())
    {
        tauri::async_runtime::spawn(async move { select_node(&app, index).await });
    } else if id == "subscription.refresh" {
        spawn_refresh_subscription(app);
    } else if id == "settings.open" {
        show_settings_window(&app);
    } else if id == "app.quit" {
        tauri::async_runtime::spawn(async move { quit(&app).await });
    }
}

fn show_settings_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("settings") {
        let _ = window.emit("settings-opened", ());
        let _ = window.center();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn set_mode_checkmarks(app: &AppHandle, selected: ProxyMode) -> Result<()> {
    let state = app.state::<AppState>();
    let items = state
        .mode_items
        .lock()
        .map_err(|_| anyhow!("代理模式菜单状态不可用"))?
        .clone();
    for (mode, item) in items.iter() {
        item.set_checked(*mode == selected)?;
    }
    Ok(())
}

fn parse_mode(value: &str) -> Option<ProxyMode> {
    match value {
        "off" => Some(ProxyMode::Off),
        "manual" => Some(ProxyMode::Manual),
        "automatic" => Some(ProxyMode::Automatic),
        "global" => Some(ProxyMode::Global),
        _ => None,
    }
}

fn tray_menu(
    app: &AppHandle,
    runtime: &Runtime,
    texts: &Texts,
) -> Result<(Menu<tauri::Wry>, ModeMenuItems)> {
    let mode = runtime.settings.mode;
    let proxy_modes_enabled = runtime.settings.has_subscription();
    let off = CheckMenuItem::with_id(
        app,
        "mode.off",
        texts.get("mode_off"),
        true,
        mode == ProxyMode::Off,
        None::<&str>,
    )?;
    let manual = CheckMenuItem::with_id(
        app,
        "mode.manual",
        texts.get("mode_manual"),
        proxy_modes_enabled,
        mode == ProxyMode::Manual,
        None::<&str>,
    )?;
    let automatic = CheckMenuItem::with_id(
        app,
        "mode.automatic",
        texts.get("mode_auto"),
        proxy_modes_enabled,
        mode == ProxyMode::Automatic,
        None::<&str>,
    )?;
    let global = CheckMenuItem::with_id(
        app,
        "mode.global",
        texts.get("mode_global"),
        proxy_modes_enabled,
        mode == ProxyMode::Global,
        None::<&str>,
    )?;
    let modes = Submenu::with_items(
        app,
        texts.get("proxy_mode"),
        true,
        &[&off, &manual, &automatic, &global],
    )?;
    let mode_items = vec![
        (ProxyMode::Off, off),
        (ProxyMode::Manual, manual),
        (ProxyMode::Automatic, automatic),
        (ProxyMode::Global, global),
    ];

    let refresh = MenuItem::with_id(
        app,
        "subscription.refresh",
        texts.get("refresh_subscription"),
        proxy_modes_enabled && !runtime.subscription_loading,
        None::<&str>,
    )?;
    let separator = PredefinedMenuItem::separator(app)?;
    let mut node_items: Vec<Box<dyn tauri::menu::IsMenuItem<tauri::Wry>>> =
        vec![Box::new(refresh), Box::new(separator)];
    if runtime.nodes.is_empty() {
        let label = if runtime.subscription_loading {
            texts.get("loading_nodes")
        } else {
            texts.get("no_nodes")
        };
        node_items.push(Box::new(MenuItem::new(app, label, false, None::<&str>)?));
    } else {
        for (index, node) in runtime.nodes.iter().enumerate() {
            let checked = runtime.settings.selected_node.as_deref() == Some(node.name.as_str())
                || (runtime.settings.selected_node.is_none() && index == 0);
            node_items.push(Box::new(CheckMenuItem::with_id(
                app,
                format!("node.{index}"),
                &node.name,
                true,
                checked,
                None::<&str>,
            )?));
        }
    }
    let refs: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> =
        node_items.iter().map(Box::as_ref).collect();
    let nodes = Submenu::with_items(app, texts.get("nodes"), true, &refs)?;
    let settings = MenuItem::with_id(
        app,
        "settings.open",
        texts.get("settings"),
        true,
        None::<&str>,
    )?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "app.quit", texts.get("quit"), true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&modes, &nodes, &settings])?;
    if !runtime.status.is_empty() {
        let status = Submenu::with_id(app, "status", texts.get("status"), true)?;
        for line in runtime
            .status
            .lines()
            .filter(|line| !line.trim().is_empty())
        {
            status.append(&MenuItem::new(app, line, false, None::<&str>)?)?;
        }
        menu.append(&status)?;
    }
    menu.append(&separator)?;
    menu.append(&quit)?;
    Ok((menu, mode_items))
}

fn tooltip(runtime: &Runtime, texts: &Texts) -> String {
    let mode = match runtime.settings.mode {
        ProxyMode::Off => texts.get("mode_off"),
        ProxyMode::Manual => texts.get("mode_manual"),
        ProxyMode::Automatic => texts.get("mode_auto"),
        ProxyMode::Global => texts.get("mode_global"),
    };
    let base = match runtime.selected_node() {
        Some(node) => format!("ProxyBar — {mode} — {}", node.name),
        None => format!("ProxyBar — {mode}"),
    };
    if !runtime.status.is_empty() {
        format!("{base} — {}", runtime.status)
    } else {
        base
    }
}

fn tray_icon(paths: &AppPaths, mode: ProxyMode) -> Result<Image<'static>> {
    let base_name = match mode {
        ProxyMode::Off => "tray-off.png",
        ProxyMode::Manual => "tray-manual.png",
        ProxyMode::Automatic => "tray-auto.png",
        ProxyMode::Global => "tray-global.png",
    };
    let themed_name = if should_use_light_tray_icon() {
        base_name.replace(".png", "-light.png")
    } else {
        base_name.to_owned()
    };
    let themed_path = paths.tray.join(&themed_name);
    let path = if themed_path.exists() {
        themed_path
    } else {
        paths.tray.join(base_name)
    };
    Image::from_path(path).map_err(Into::into)
}

#[cfg(target_os = "windows")]
fn should_use_light_tray_icon() -> bool {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    const CREATE_NO_WINDOW: u32 = 0x08000000;
    let output = Command::new("reg")
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
            "/v",
            "SystemUsesLightTheme",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output();

    let Ok(output) = output else {
        return false;
    };
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .find(|line| line.contains("SystemUsesLightTheme"))
        .is_some_and(|line| line.split_whitespace().last() == Some("0x0"))
}

#[cfg(not(target_os = "windows"))]
fn should_use_light_tray_icon() -> bool {
    false
}

#[cfg(target_os = "windows")]
fn spawn_tray_theme_watcher(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut uses_light_icon = should_use_light_tray_icon();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            let state = app.state::<AppState>();
            if state.quitting.load(Ordering::SeqCst) {
                break;
            }

            let current = should_use_light_tray_icon();
            if current != uses_light_icon {
                uses_light_icon = current;
                let _ = rebuild_tray(&app).await;
            }
        }
    });
}

#[cfg(not(target_os = "windows"))]
fn spawn_tray_theme_watcher(_app: AppHandle) {}

fn restart_delay(attempts: u32) -> Duration {
    Duration::from_secs(match attempts {
        0 => 2,
        1 => 2,
        2 => 5,
        3 => 10,
        _ => 30,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProxyHealthCheck {
    Skip,
    Restart,
    CheckPort(u16),
}

fn proxy_health_check(
    mode: ProxyMode,
    restart_pending: bool,
    port: u16,
    process_exited: Option<bool>,
) -> ProxyHealthCheck {
    if mode == ProxyMode::Off {
        ProxyHealthCheck::Skip
    } else if restart_pending {
        ProxyHealthCheck::Restart
    } else {
        match process_exited {
            Some(true) => ProxyHealthCheck::Restart,
            Some(false) => ProxyHealthCheck::CheckPort(port),
            None => ProxyHealthCheck::Skip,
        }
    }
}

fn spawn_proxy_watcher(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            let delay = {
                let state = app.state::<AppState>();
                let runtime = state.inner.lock().await;
                if runtime.restart_pending {
                    restart_delay(runtime.restart_attempts)
                } else {
                    Duration::from_secs(2)
                }
            };
            tokio::time::sleep(delay).await;

            let state = app.state::<AppState>();
            if state.quitting.load(Ordering::SeqCst) {
                break;
            }
            let check = {
                let mut runtime = state.inner.lock().await;
                let exited = runtime
                    .process
                    .as_mut()
                    .map(|process| proxy::process_exited(process).unwrap_or(true));
                proxy_health_check(
                    runtime.settings.mode,
                    runtime.restart_pending,
                    runtime.port,
                    exited,
                )
            };
            let unhealthy = match check {
                ProxyHealthCheck::Skip => false,
                ProxyHealthCheck::Restart => true,
                ProxyHealthCheck::CheckPort(port) => !proxy::socks_port_ready(port).await,
            };
            if unhealthy {
                let _ = restart_unhealthy_proxy(&app).await;
            }
        }
    });
}

async fn rebuild_tray(app: &AppHandle) -> Result<()> {
    let state = app.state::<AppState>();
    let runtime = state.inner.lock().await;
    let (menu, mode_items) = tray_menu(app, &runtime, &state.texts)?;
    let icon = tray_icon(&state.paths, runtime.settings.mode)?;
    let tray = app.tray_by_id(TRAY_ID).context("tray is unavailable")?;
    tray.set_menu(Some(menu))?;
    tray.set_icon_with_as_template(Some(icon), cfg!(target_os = "macos"))?;
    tray.set_tooltip(Some(tooltip(&runtime, &state.texts)))?;
    *state
        .mode_items
        .lock()
        .map_err(|_| anyhow!("代理模式菜单状态不可用"))? = mode_items;
    Ok(())
}

async fn switch_mode(app: &AppHandle, mode: ProxyMode) -> Result<()> {
    let state = app.state::<AppState>();
    let _transition = state.transition.lock().await;
    let old_process = {
        let mut runtime = state.inner.lock().await;
        if !runtime.settings.mode_is_available(mode) {
            return Err(anyhow!("请先在设置中填写订阅地址"));
        }
        runtime.settings.mode = mode;
        runtime.restart_pending = false;
        runtime.restart_attempts = 0;
        runtime.status.clear();
        runtime.process.take()
    };
    proxy::stop_sing_box(&state.paths, old_process).await?;

    if mode == ProxyMode::Off {
        let settings = { state.inner.lock().await.settings.clone() };
        persist_settings(&state.paths, &settings).await?;
        rebuild_tray(app).await?;
        return Ok(());
    }

    let node = state
        .inner
        .lock()
        .await
        .selected_node()
        .ok_or_else(|| anyhow!("请先刷新并选择节点"))?;
    let configured_port = state.inner.lock().await.settings.proxy_port;
    let port = proxy::available_port(configured_port).await?;
    let process = proxy::start_sing_box(&state.paths, &node, port, mode).await?;
    {
        let mut runtime = state.inner.lock().await;
        runtime.port = port;
        runtime.process = Some(process);
        runtime.settings.selected_node = Some(node.name.clone());
    }
    let settings = { state.inner.lock().await.settings.clone() };
    persist_settings(&state.paths, &settings).await?;
    rebuild_tray(app).await?;
    Ok(())
}

async fn restart_unhealthy_proxy(app: &AppHandle) -> Result<()> {
    let state = app.state::<AppState>();
    let _transition = state.transition.lock().await;
    if state.quitting.load(Ordering::SeqCst) {
        return Ok(());
    }

    let (pending, exited, port) = {
        let mut runtime = state.inner.lock().await;
        if runtime.settings.mode == ProxyMode::Off {
            return Ok(());
        }
        let pending = runtime.restart_pending;
        let port = runtime.port;
        let exited = match runtime.process.as_mut() {
            Some(process) => proxy::process_exited(process).unwrap_or(true),
            None if pending => true,
            None => return Ok(()),
        };
        (pending, exited, port)
    };
    if !pending && !exited && proxy::socks_port_ready(port).await {
        return Ok(());
    }

    let (old_process, node, mode, configured_port, attempt) = {
        let mut runtime = state.inner.lock().await;
        if runtime.settings.mode == ProxyMode::Off {
            return Ok(());
        }
        runtime.restart_pending = true;
        runtime.restart_attempts = runtime.restart_attempts.saturating_add(1);
        let attempt = runtime.restart_attempts;
        runtime.status = format!("{} ({attempt})", state.texts.get("proxy_restarting"));
        (
            runtime.process.take(),
            runtime.selected_node(),
            runtime.settings.mode,
            runtime.settings.proxy_port,
            attempt,
        )
    };
    rebuild_tray(app).await?;
    proxy::stop_sing_box(&state.paths, old_process).await?;

    let result = async {
        let node = node.ok_or_else(|| anyhow!("请先刷新并选择节点"))?;
        let port = proxy::available_port(configured_port).await?;
        let process = proxy::start_sing_box(&state.paths, &node, port, mode).await?;
        let mut runtime = state.inner.lock().await;
        runtime.port = port;
        runtime.process = Some(process);
        runtime.restart_pending = false;
        runtime.restart_attempts = 0;
        runtime.status.clear();
        Ok::<_, anyhow::Error>(())
    }
    .await;

    if let Err(error) = result {
        let mut runtime = state.inner.lock().await;
        runtime.restart_pending = true;
        runtime.status = format!(
            "{} ({attempt}): {error}",
            state.texts.get("proxy_restart_failed")
        );
        drop(runtime);
        let _ = rebuild_tray(app).await;
        return Err(error);
    }
    rebuild_tray(app).await?;
    Ok(())
}

async fn select_node(app: &AppHandle, index: usize) {
    let state = app.state::<AppState>();
    let mode = {
        let mut runtime = state.inner.lock().await;
        let Some(node) = runtime.nodes.get(index) else {
            return;
        };
        runtime.settings.selected_node = Some(node.name.clone());
        runtime.settings.mode
    };
    if mode == ProxyMode::Off {
        let settings = { state.inner.lock().await.settings.clone() };
        let _ = persist_settings(&state.paths, &settings).await;
        let _ = rebuild_tray(app).await;
    } else if let Err(error) = switch_mode(app, mode).await {
        recover_off(app).await;
        set_status(app, format!("{error:#}")).await;
    }
}

async fn recover_off(app: &AppHandle) {
    let state = app.state::<AppState>();
    let _transition = state.transition.lock().await;
    let (process, settings) = {
        let mut runtime = state.inner.lock().await;
        let process = runtime.process.take();
        runtime.settings.mode = ProxyMode::Off;
        runtime.restart_pending = false;
        runtime.restart_attempts = 0;
        (process, runtime.settings.clone())
    };
    let _ = proxy::stop_sing_box(&state.paths, process).await;
    let _ = persist_settings(&state.paths, &settings).await;
    let _ = rebuild_tray(app).await;
}

fn spawn_refresh_subscription(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        if let Err(error) = refresh_subscription(&app).await {
            set_status(&app, format!("{error:#}")).await;
        }
    });
}

async fn refresh_subscription(app: &AppHandle) -> Result<()> {
    let state = app.state::<AppState>();
    let url = {
        let mut runtime = state.inner.lock().await;
        if runtime.subscription_loading {
            return Ok(());
        }
        if !runtime.settings.has_subscription() {
            return Err(anyhow!("请先在设置中填写订阅地址"));
        }
        runtime.subscription_loading = true;
        runtime.settings.subscription_url.clone()
    };
    rebuild_tray(app).await?;

    let result = async {
        let body = reqwest::Client::builder()
            .user_agent("ProxyBar/0.2")
            .build()?
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        let nodes = subscription::parse(&body);
        if nodes.is_empty() {
            return Err(anyhow!("订阅中没有可用的 VLESS 节点"));
        }
        tokio::fs::write(&state.paths.subscription_cache, body).await?;
        let settings = {
            let mut runtime = state.inner.lock().await;
            runtime.nodes = nodes;
            if runtime
                .settings
                .selected_node
                .as_ref()
                .is_none_or(|selected| !runtime.nodes.iter().any(|node| &node.name == selected))
            {
                runtime.settings.selected_node =
                    runtime.nodes.first().map(|node| node.name.clone());
            }
            runtime.settings.clone()
        };
        persist_settings(&state.paths, &settings).await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    state.inner.lock().await.subscription_loading = false;
    rebuild_tray(app).await?;
    result
}

async fn set_status(app: &AppHandle, status: String) {
    let state = app.state::<AppState>();
    eprintln!("{status}");
    use tokio::io::AsyncWriteExt;
    if let Ok(mut log) = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(state.paths.data_dir.join("proxybar.log"))
        .await
    {
        let _ = log.write_all(format!("{status}\n").as_bytes()).await;
    }
    state.inner.lock().await.status = status;
    let _ = rebuild_tray(app).await;
}

async fn quit(app: &AppHandle) {
    let state = app.state::<AppState>();
    if state.quitting.swap(true, Ordering::SeqCst) {
        return;
    }
    let _transition = state.transition.lock().await;
    let process = {
        let mut runtime = state.inner.lock().await;
        runtime.restart_pending = false;
        runtime.process.take()
    };
    let _ = proxy::stop_sing_box(&state.paths, process).await;
    #[cfg(target_os = "macos")]
    privileged::shutdown();
    app.exit(0);
}

async fn persist_settings(paths: &AppPaths, settings: &Settings) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(settings)?;
    tokio::fs::write(&paths.settings, bytes).await?;
    Ok(())
}

fn load_settings(paths: &AppPaths) -> Settings {
    if let Ok(bytes) = fs::read(&paths.settings) {
        if let Ok(mut settings) = serde_json::from_slice::<Settings>(&bytes) {
            settings.normalize_mode();
            return settings;
        }
    }
    let legacy = paths.data_dir.join("settings.conf");
    let Ok(contents) = fs::read_to_string(legacy) else {
        return Settings::default();
    };
    let mut settings = Settings::default();
    for line in contents.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "subscription" => settings.subscription_url = value.trim().into(),
            "port" => {
                if let Ok(port) = value.trim().parse::<u16>() {
                    settings.proxy_port = port;
                }
            }
            "mode" => {
                settings.mode = match value.trim() {
                    "manual" => ProxyMode::Manual,
                    "gfwlist" => ProxyMode::Automatic,
                    "global" => ProxyMode::Global,
                    _ => ProxyMode::Off,
                }
            }
            _ => {}
        }
    }
    settings.normalize_mode();
    settings
}

fn resolve_paths(app: &AppHandle) -> Result<AppPaths> {
    let data_dir = dirs::data_local_dir()
        .context("local data directory is unavailable")?
        .join("ProxyBar");
    let bundled_assets = app.path().resource_dir()?.join("assets");
    let source_assets = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("project root is unavailable")?
        .join("assets");
    let (common, platform) = if bundled_assets.is_dir() {
        (
            bundled_assets.join("common"),
            bundled_assets.join("platform"),
        )
    } else {
        (
            source_assets.join("common"),
            source_assets.join("platforms").join(std::env::consts::OS),
        )
    };
    let sing_box_name = sing_box_binary_name(std::env::consts::OS, std::env::consts::ARCH)
        .ok_or_else(|| {
            anyhow!(
                "unsupported sing-box platform: {}/{}",
                std::env::consts::OS,
                std::env::consts::ARCH
            )
        })?;
    Ok(AppPaths {
        settings: data_dir.join("settings.json"),
        subscription_cache: data_dir.join("subscription.cache"),
        sing_box_config: data_dir.join("sing-box.json"),
        sing_box_cache: data_dir.join("sing-box-cache.db"),
        sing_box_log: data_dir.join("sing-box.log"),
        sing_box_pid: data_dir.join("sing-box.pid"),
        sing_box: platform.join(sing_box_name),
        locales: common.join("locales"),
        tray: common.join("tray"),
        data_dir,
    })
}

fn ensure_executable(path: &Path) -> Result<()> {
    if !path.is_file() {
        return Err(anyhow!("missing bundled resource: {}", path.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)?.permissions();
        if permissions.mode() & 0o111 == 0 {
            permissions.set_mode(permissions.mode() | 0o755);
            fs::set_permissions(path, permissions)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        proxy_health_check, restart_delay, validate_http_url, validate_optional_http_url,
        ProxyHealthCheck,
    };
    use crate::model::ProxyMode;
    use std::time::Duration;

    #[test]
    fn failed_switch_remains_visible_after_recovery_to_off() {
        let texts = crate::localization::Texts::load(std::path::Path::new("/nonexistent/locales"));
        let mut runtime = crate::model::Runtime {
            settings: crate::model::Settings::default(),
            nodes: Vec::new(),
            process: None,
            port: 10850,
            subscription_loading: false,
            status: "administrator helper disconnected".into(),
            restart_pending: false,
            restart_attempts: 0,
        };
        assert!(super::tooltip(&runtime, &texts).contains(&runtime.status));
        runtime.status.clear();
        assert!(!super::tooltip(&runtime, &texts).contains("disconnected"));
    }

    #[test]
    fn empty_subscription_url_is_valid() {
        assert_eq!(
            validate_optional_http_url("   ", "订阅地址"),
            Ok(String::new())
        );
    }

    #[test]
    fn configured_subscription_url_still_requires_http() {
        assert!(validate_optional_http_url("ftp://example.com/sub", "订阅地址").is_err());
        assert!(validate_http_url("https://example.com/sub", "订阅地址").is_ok());
    }

    #[test]
    fn proxy_restart_delay_is_bounded() {
        assert_eq!(restart_delay(0), Duration::from_secs(2));
        assert_eq!(restart_delay(2), Duration::from_secs(5));
        assert_eq!(restart_delay(3), Duration::from_secs(10));
        assert_eq!(restart_delay(4), Duration::from_secs(30));
        assert_eq!(restart_delay(u32::MAX), Duration::from_secs(30));
    }

    #[test]
    fn proxy_health_check_only_restarts_active_unhealthy_processes() {
        assert_eq!(
            proxy_health_check(ProxyMode::Off, true, 10850, Some(true)),
            ProxyHealthCheck::Skip
        );
        assert_eq!(
            proxy_health_check(ProxyMode::Manual, false, 10850, None),
            ProxyHealthCheck::Skip
        );
        assert_eq!(
            proxy_health_check(ProxyMode::Manual, false, 10850, Some(false)),
            ProxyHealthCheck::CheckPort(10850)
        );
        assert_eq!(
            proxy_health_check(ProxyMode::Automatic, false, 10850, Some(true)),
            ProxyHealthCheck::Restart
        );
        assert_eq!(
            proxy_health_check(ProxyMode::Global, true, 10850, None),
            ProxyHealthCheck::Restart
        );
    }
}
