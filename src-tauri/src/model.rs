use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::process::Child;

pub const PORT_START: u16 = 10_850;

pub fn sing_box_binary_name(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("macos", "x86_64") => Some("sing-box-x86_64"),
        ("macos", "aarch64") => Some("sing-box-aarch64"),
        ("windows", "x86_64") => Some("sing-box.exe"),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyMode {
    #[default]
    Off,
    Manual,
    Automatic,
    Global,
}

impl ProxyMode {
    pub fn uses_tun(self) -> bool {
        matches!(self, Self::Automatic | Self::Global)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Settings {
    pub subscription_url: String,
    pub proxy_port: u16,
    pub selected_node: Option<String>,
    pub mode: ProxyMode,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            subscription_url: String::new(),
            proxy_port: PORT_START,
            selected_node: None,
            mode: ProxyMode::Off,
        }
    }
}

impl Settings {
    pub fn has_subscription(&self) -> bool {
        !self.subscription_url.trim().is_empty()
    }

    pub fn mode_is_available(&self, mode: ProxyMode) -> bool {
        mode == ProxyMode::Off || self.has_subscription()
    }

    pub fn normalize_mode(&mut self) {
        if !self.mode_is_available(self.mode) {
            self.mode = ProxyMode::Off;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_settings_receive_new_defaults() {
        let settings: Settings = serde_json::from_str(
            r#"{"subscription_url":"https://example.com/sub","selected_node":null,"mode":"off"}"#,
        )
        .unwrap();
        assert_eq!(settings.proxy_port, PORT_START);
    }

    #[test]
    fn missing_subscription_only_allows_off_mode() {
        let mut settings = Settings {
            mode: ProxyMode::Global,
            ..Settings::default()
        };

        assert!(!settings.has_subscription());
        assert!(settings.mode_is_available(ProxyMode::Off));
        assert!(!settings.mode_is_available(ProxyMode::Manual));
        assert!(!settings.mode_is_available(ProxyMode::Automatic));
        assert!(!settings.mode_is_available(ProxyMode::Global));

        settings.normalize_mode();
        assert_eq!(settings.mode, ProxyMode::Off);
    }

    #[test]
    fn configured_subscription_allows_proxy_modes() {
        let settings = Settings {
            subscription_url: "https://example.com/sub".into(),
            ..Settings::default()
        };

        assert!(settings.has_subscription());
        assert!(settings.mode_is_available(ProxyMode::Manual));
        assert!(settings.mode_is_available(ProxyMode::Automatic));
        assert!(settings.mode_is_available(ProxyMode::Global));
    }

    #[test]
    fn selects_sing_box_for_each_supported_macos_architecture() {
        assert_eq!(
            sing_box_binary_name("macos", "x86_64"),
            Some("sing-box-x86_64")
        );
        assert_eq!(
            sing_box_binary_name("macos", "aarch64"),
            Some("sing-box-aarch64")
        );
    }

    #[test]
    fn selects_only_the_supported_windows_architecture() {
        assert_eq!(
            sing_box_binary_name("windows", "x86_64"),
            Some("sing-box.exe")
        );
        assert_eq!(sing_box_binary_name("windows", "aarch64"), None);
        assert_eq!(sing_box_binary_name("linux", "x86_64"), None);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Transport {
    Tcp,
    Ws,
    Grpc,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Node {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub uuid: String,
    pub transport: Transport,
    pub tls: bool,
    pub sni: String,
    pub path: String,
    pub host_header: String,
    pub service_name: String,
}

#[derive(Clone, Debug)]
pub struct AppPaths {
    pub data_dir: PathBuf,
    pub settings: PathBuf,
    pub subscription_cache: PathBuf,
    pub sing_box_config: PathBuf,
    pub sing_box_cache: PathBuf,
    pub sing_box_log: PathBuf,
    pub sing_box_pid: PathBuf,
    pub sing_box: PathBuf,
    pub locales: PathBuf,
    pub tray: PathBuf,
}

pub enum ManagedProcess {
    Child(Child),
    #[cfg(target_os = "macos")]
    Elevated {
        pid: u32,
    },
}

pub struct Runtime {
    pub settings: Settings,
    pub nodes: Vec<Node>,
    pub process: Option<ManagedProcess>,
    pub port: u16,
    pub subscription_loading: bool,
    pub status: String,
}

impl Runtime {
    pub fn selected_node(&self) -> Option<Node> {
        let selected = self.settings.selected_node.as_deref();
        selected
            .and_then(|name| self.nodes.iter().find(|node| node.name == name))
            .or_else(|| self.nodes.first())
            .cloned()
    }
}
