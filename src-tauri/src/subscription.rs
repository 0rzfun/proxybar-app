use crate::model::{Node, Transport};

pub const MAX_NODES: usize = 20;

pub fn parse(body: &str) -> Vec<Node> {
    body.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with(';'))
        .filter_map(parse_line)
        .take(MAX_NODES)
        .collect()
}

pub fn parse_line(line: &str) -> Option<Node> {
    let (name, spec) = line.split_once('=')?;
    let name = clean(name);
    let parts: Vec<_> = spec.split(',').map(clean).collect();
    if name.is_empty() || parts.len() < 4 || !parts[0].eq_ignore_ascii_case("vless") {
        return None;
    }

    let host = parts[1];
    let port = parts[2].parse::<u16>().ok()?;
    let uuid = parts[3];
    if host.is_empty() || uuid.is_empty() || port == 0 {
        return None;
    }

    let mut node = Node {
        name: name.to_owned(),
        host: host.to_owned(),
        port,
        uuid: uuid.to_owned(),
        transport: Transport::Tcp,
        tls: false,
        sni: String::new(),
        path: String::new(),
        host_header: String::new(),
        service_name: String::new(),
    };

    for option in &parts[4..] {
        let Some((key, value)) = option.split_once('=') else {
            continue;
        };
        let key = clean(key);
        let value = clean(value);
        if key.eq_ignore_ascii_case("transport") {
            node.transport = if value.eq_ignore_ascii_case("ws") {
                Transport::Ws
            } else if value.eq_ignore_ascii_case("grpc") {
                Transport::Grpc
            } else {
                Transport::Tcp
            };
        } else if key.eq_ignore_ascii_case("over-tls") || key.eq_ignore_ascii_case("tls") {
            node.tls = value.eq_ignore_ascii_case("true") || value == "1";
        } else if key.eq_ignore_ascii_case("sni") || key.eq_ignore_ascii_case("server-name") {
            node.sni = value.to_owned();
        } else if key.eq_ignore_ascii_case("path") {
            node.path = value.to_owned();
        } else if key.eq_ignore_ascii_case("host") {
            node.host_header = value.to_owned();
        } else if key.eq_ignore_ascii_case("service-name")
            || key.eq_ignore_ascii_case("serviceName")
        {
            node.service_name = value.to_owned();
        }
    }
    Some(node)
}

fn clean(value: &str) -> &str {
    let value = value.trim().trim_matches('"').trim();
    if value.len() >= 2
        && ((value.starts_with('\'') && value.ends_with('\''))
            || (value.starts_with('"') && value.ends_with('"')))
    {
        value[1..value.len() - 1].trim()
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_loon_vless() {
        let node = parse_line("🇺🇸 Demo = VLESS,example.com,443,uuid-1,transport=ws,path=/vless,over-tls=true,sni=edge.example.com").unwrap();
        assert_eq!(node.name, "🇺🇸 Demo");
        assert_eq!(node.transport, Transport::Ws);
        assert!(node.tls);
        assert_eq!(node.sni, "edge.example.com");
    }

    #[test]
    fn ignores_unsupported_or_broken_lines() {
        assert!(parse_line("demo = Shadowsocks,host,443,password").is_none());
        assert!(parse_line("broken").is_none());
    }
}
