use crate::model::{Node, ProxyMode, Transport};
use serde_json::{json, Map, Value};
use std::path::Path;

const GEOIP_CN_URL: &str =
    "https://raw.githubusercontent.com/SagerNet/sing-geoip/rule-set/geoip-cn.srs";
const GEOSITE_CN_URL: &str =
    "https://raw.githubusercontent.com/SagerNet/sing-geosite/rule-set/geosite-cn.srs";

pub fn config(node: &Node, listen_port: u16, mode: ProxyMode, cache_path: &Path) -> Value {
    let mut inbounds = vec![json!({
        "type": "socks",
        "tag": "socks-in",
        "listen": "127.0.0.1",
        "listen_port": listen_port
    })];
    if mode.uses_tun() {
        inbounds.push(json!({
            "type": "tun",
            "tag": "tun-in",
            "address": ["172.19.0.1/30", "fdfe:dcba:9876::1/126"],
            "mtu": 9000,
            "auto_route": true,
            "strict_route": true,
            "stack": "mixed"
        }));
    }

    let mut vless = Map::from_iter([
        ("type".into(), json!("vless")),
        ("tag".into(), json!("proxy")),
        ("server".into(), json!(node.host)),
        ("server_port".into(), json!(node.port)),
        ("uuid".into(), json!(node.uuid)),
        ("domain_resolver".into(), json!("local-dns")),
    ]);
    if node.tls {
        vless.insert(
            "tls".into(),
            json!({
                "enabled": true,
                "server_name": if node.sni.is_empty() { &node.host } else { &node.sni }
            }),
        );
    }
    match node.transport {
        Transport::Ws => {
            vless.insert(
                "transport".into(),
                json!({
                    "type": "ws",
                    "path": if node.path.is_empty() { "/" } else { &node.path },
                    "headers": {
                        "Host": if node.host_header.is_empty() { &node.host } else { &node.host_header }
                    }
                }),
            );
        }
        Transport::Grpc => {
            vless.insert(
                "transport".into(),
                json!({ "type": "grpc", "service_name": node.service_name }),
            );
        }
        Transport::Tcp => {}
    }

    let mut root = Map::from_iter([
        ("log".into(), json!({ "level": "warn", "timestamp": true })),
        ("dns".into(), dns_config(mode)),
        ("inbounds".into(), Value::Array(inbounds)),
        (
            "outbounds".into(),
            json!([Value::Object(vless), { "type": "direct", "tag": "direct" }]),
        ),
        ("route".into(), route_config(mode)),
    ]);
    if mode == ProxyMode::Automatic {
        root.insert(
            "experimental".into(),
            json!({
                "cache_file": {
                    "enabled": true,
                    "path": cache_path
                }
            }),
        );
    }
    Value::Object(root)
}

fn dns_config(mode: ProxyMode) -> Value {
    let local = json!({
        "type": "udp",
        "tag": "local-dns",
        "server": "223.5.5.5"
    });
    if !mode.uses_tun() {
        return json!({
            "servers": [local],
            "final": "local-dns",
            "strategy": "prefer_ipv4"
        });
    }

    let mut rules = Vec::new();
    if mode == ProxyMode::Automatic {
        rules.push(json!({
            "rule_set": "geosite-cn",
            "action": "route",
            "server": "local-dns"
        }));
    }
    json!({
        "servers": [local, {
            "type": "tls",
            "tag": "remote-dns",
            "server": "1.1.1.1",
            "server_port": 853,
            "detour": "proxy",
            "tls": {
                "enabled": true,
                "server_name": "cloudflare-dns.com"
            }
        }],
        "rules": rules,
        "final": "remote-dns",
        "strategy": "prefer_ipv4",
        "reverse_mapping": true
    })
}

fn route_config(mode: ProxyMode) -> Value {
    let mut rules = Vec::new();
    if mode.uses_tun() {
        rules.extend([
            json!({ "action": "sniff" }),
            json!({ "protocol": "dns", "action": "hijack-dns" }),
            json!({
                "ip_is_private": true,
                "action": "route",
                "outbound": "direct"
            }),
        ]);
    }
    let mut rule_sets = Vec::new();
    if mode == ProxyMode::Automatic {
        rules.push(json!({
            "rule_set": ["geoip-cn", "geosite-cn"],
            "action": "route",
            "outbound": "direct"
        }));
        rule_sets.extend([
            json!({
                "type": "remote",
                "tag": "geoip-cn",
                "format": "binary",
                "url": GEOIP_CN_URL,
                "download_detour": "proxy"
            }),
            json!({
                "type": "remote",
                "tag": "geosite-cn",
                "format": "binary",
                "url": GEOSITE_CN_URL,
                "download_detour": "proxy"
            }),
        ]);
    }
    json!({
        "rules": rules,
        "rule_set": rule_sets,
        "final": "proxy",
        "auto_detect_interface": mode.uses_tun(),
        "default_domain_resolver": "local-dns"
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model::sing_box_binary_name, subscription::parse_line};
    use std::{fs, process::Command};

    fn test_node() -> Node {
        parse_line("Demo = VLESS,example.com,443,123e4567-e89b-12d3-a456-426614174000,transport=ws,path=/vless,over-tls=true,sni=edge.example.com").unwrap()
    }

    #[test]
    fn manual_mode_only_exposes_socks() {
        let value = config(
            &test_node(),
            10_850,
            ProxyMode::Manual,
            Path::new("cache.db"),
        );
        assert_eq!(value["inbounds"].as_array().unwrap().len(), 1);
        assert_eq!(value["inbounds"][0]["type"], "socks");
        assert_eq!(value["outbounds"][0]["transport"]["type"], "ws");
        assert_eq!(
            value["outbounds"][0]["tls"]["server_name"],
            "edge.example.com"
        );
        assert!(value.get("experimental").is_none());
    }

    #[test]
    fn automatic_mode_uses_tun_and_sing_box_rule_sets() {
        let value = config(
            &test_node(),
            10_850,
            ProxyMode::Automatic,
            Path::new("cache.db"),
        );
        assert_eq!(value["inbounds"][1]["type"], "tun");
        assert_eq!(value["inbounds"][1]["auto_route"], true);
        assert_eq!(value["route"]["rule_set"].as_array().unwrap().len(), 2);
        assert_eq!(value["experimental"]["cache_file"]["path"], "cache.db");
    }

    #[test]
    fn global_mode_uses_tun_without_regional_rule_sets() {
        let value = config(
            &test_node(),
            10_850,
            ProxyMode::Global,
            Path::new("cache.db"),
        );
        assert_eq!(value["inbounds"][1]["type"], "tun");
        assert!(value["route"]["rule_set"].as_array().unwrap().is_empty());
        assert!(value.get("experimental").is_none());
    }

    #[test]
    fn bundled_sing_box_accepts_every_generated_mode() {
        let project_root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let binary_name = sing_box_binary_name(std::env::consts::OS, std::env::consts::ARCH)
            .expect("test target must have a bundled sing-box");
        let binary = project_root
            .join("assets/platforms")
            .join(std::env::consts::OS)
            .join(binary_name);
        let temp = std::env::temp_dir().join(format!(
            "proxybar-sing-box-config-{}-{}.json",
            std::process::id(),
            std::env::consts::ARCH
        ));

        for mode in [ProxyMode::Manual, ProxyMode::Automatic, ProxyMode::Global] {
            let value = config(&test_node(), 10_850, mode, Path::new("cache.db"));
            fs::write(&temp, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
            let output = Command::new(&binary)
                .args(["check", "-c"])
                .arg(&temp)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "sing-box rejected {mode:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let _ = fs::remove_file(temp);
    }
}
