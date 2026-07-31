use std::{collections::HashMap, fs, path::Path};

#[derive(Clone)]
pub struct Texts {
    values: HashMap<String, String>,
}

impl Texts {
    pub fn load(locales_dir: &Path) -> Self {
        let mut values = defaults();
        apply_file(&mut values, &locales_dir.join("en.conf"));

        let locale = std::env::var("PROXYBAR_LOCALE")
            .ok()
            .or_else(sys_locale::get_locale)
            .unwrap_or_else(|| "en".into())
            .replace('_', "-");
        for candidate in locale_candidates(&locale) {
            if candidate != "en" {
                apply_file(&mut values, &locales_dir.join(format!("{candidate}.conf")));
            }
        }
        Self { values }
    }

    pub fn get<'a>(&'a self, key: &'a str) -> &'a str {
        self.values.get(key).map(String::as_str).unwrap_or(key)
    }
}

fn locale_candidates(locale: &str) -> Vec<String> {
    let normalized = locale.replace('_', "-");
    let parts: Vec<&str> = normalized
        .split('-')
        .filter(|part| !part.is_empty())
        .collect();
    let language = parts.first().copied().unwrap_or("en").to_ascii_lowercase();
    let mut candidates = vec![language.clone()];

    if language == "zh" {
        // The bundled Simplified Chinese translation is also the closest fallback for
        // zh-Hans and Chinese locales such as zh-SG. More specific files can override it.
        push_unique(&mut candidates, "zh-CN".into());
        let is_traditional = parts.iter().skip(1).any(|part| {
            part.eq_ignore_ascii_case("Hant")
                || matches!(part.to_ascii_uppercase().as_str(), "TW" | "HK" | "MO")
        });
        if is_traditional {
            push_unique(&mut candidates, "zh-TW".into());
        }
    }

    if parts.len() >= 2 && parts[1].len() == 4 {
        let script = parts[1];
        let script = format!(
            "{}{}",
            script[..1].to_ascii_uppercase(),
            script[1..].to_ascii_lowercase()
        );
        push_unique(&mut candidates, format!("{language}-{script}"));
    }

    push_unique(&mut candidates, canonical_locale(&parts));
    candidates
}

fn canonical_locale(parts: &[&str]) -> String {
    parts
        .iter()
        .enumerate()
        .map(|(index, part)| match index {
            0 => part.to_ascii_lowercase(),
            1 if part.len() == 4 => format!(
                "{}{}",
                part[..1].to_ascii_uppercase(),
                part[1..].to_ascii_lowercase()
            ),
            _ if part.len() == 2 || part.len() == 3 => part.to_ascii_uppercase(),
            _ => (*part).to_owned(),
        })
        .collect::<Vec<_>>()
        .join("-")
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !value.is_empty() && !values.contains(&value) {
        values.push(value);
    }
}

fn apply_file(values: &mut HashMap<String, String>, path: &Path) {
    let Ok(contents) = fs::read_to_string(path) else {
        return;
    };
    for line in contents.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            values.insert(key.trim().into(), value.trim().into());
        }
    }
}

fn defaults() -> HashMap<String, String> {
    [
        ("proxy_mode", "Proxy Mode"),
        ("mode_off", "Off"),
        ("mode_manual", "Manual"),
        ("mode_auto", "Automatic"),
        ("mode_global", "Global"),
        ("nodes", "Nodes"),
        ("refresh_subscription", "Refresh Subscription"),
        ("settings", "Settings"),
        ("loading_nodes", "Loading nodes…"),
        ("no_nodes", "No nodes; refresh subscription"),
        ("quit", "Quit"),
        ("settings_title", "Settings"),
        (
            "settings_description",
            "Configure the subscription and local SOCKS5 port",
        ),
        ("subscription_label", "Subscription URL"),
        (
            "subscription_placeholder",
            "Enter an http:// or https:// subscription URL, or leave empty",
        ),
        ("proxy_port_label", "Local proxy port"),
        ("confirm", "OK"),
        ("cancel", "Cancel"),
        ("proxy_restarting", "Proxy process exited; restarting…"),
        ("proxy_restart_failed", "Proxy restart failed"),
    ]
    .into_iter()
    .map(|(key, value)| (key.into(), value.into()))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::locale_candidates;

    #[test]
    fn simplified_chinese_regions_use_the_bundled_translation() {
        assert_eq!(
            locale_candidates("zh-Hans-SG"),
            vec!["zh", "zh-CN", "zh-Hans", "zh-Hans-SG"]
        );
        assert_eq!(locale_candidates("zh_SG"), vec!["zh", "zh-CN", "zh-SG"]);
    }

    #[test]
    fn specific_locale_overrides_language_fallback() {
        assert_eq!(locale_candidates("en-SG"), vec!["en", "en-SG"]);
        assert_eq!(
            locale_candidates("zh-Hant-HK"),
            vec!["zh", "zh-CN", "zh-TW", "zh-Hant", "zh-Hant-HK"]
        );
    }
}
