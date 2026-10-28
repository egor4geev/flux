//! The country Claude's requests would come from (part «Флаг страны», wiki ADR-041): the country
//! of the public IP address, asked of neutral services — never of Anthropic, so that a check made
//! from a blocked country doesn't reach it either. The window blocks Claude while the country is
//! on the user's list or can't be told (`region_guard` in flux-app).
//!
//! The request goes the way the CLI's would: through the proxy `claude` is given (the `env` of
//! its settings, then Flux's environment), on a connection of its own each time — a kept-alive one
//! could still run through a tunnel that is gone. A VPN that routes only some sites through itself
//! may send this check and Claude different ways; then the flag tells about the check's way.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

/// Where the network sees this computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    /// ISO 3166-1 alpha-2, upper case: "FI".
    pub country: String,
    /// The public address, when the service tells it.
    pub ip: Option<String>,
}

/// A service that tells the country of the address a request comes from.
struct Service {
    url: &'static str,
    parse: fn(&str) -> Option<Location>,
}

const SERVICES: [Service; 2] = [
    // Cloudflare's trace: `key=value` lines, the country in `loc`.
    Service {
        url: "https://www.cloudflare.com/cdn-cgi/trace",
        parse: parse_trace,
    },
    // An independent second one, for when the first can't be reached.
    Service {
        url: "https://api.country.is/",
        parse: parse_country_is,
    },
];

const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
const ANSWER_TIMEOUT: Duration = Duration::from_secs(6);
/// Both answers are a few hundred bytes.
const ANSWER_LIMIT: u64 = 16 * 1024;

/// The proxy variables `claude` reads, in its order.
const PROXY_KEYS: [&str; 4] = ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"];

/// Asks the services in turn; the first answer wins. `proxy` — as [`claude_proxy`] gives it.
/// Blocks: run it on a background thread.
pub fn detect(proxy: Option<&str>) -> Result<Location, String> {
    let agent = agent(proxy)?;
    let mut errors = Vec::new();
    for service in &SERVICES {
        match ask(&agent, service) {
            Ok(location) => return Ok(location),
            Err(err) => errors.push(err),
        }
    }
    Err(errors.join("; "))
}

fn agent(proxy: Option<&str>) -> Result<ureq::Agent, String> {
    let proxy = match proxy {
        Some(proxy) => {
            Some(ureq::Proxy::new(proxy).map_err(|err| format!("proxy {proxy}: {err}"))?)
        }
        None => None,
    };
    Ok(ureq::Agent::config_builder()
        .user_agent("Flux")
        .http_status_as_error(false)
        // Not the agent's default (`ALL_PROXY` first): exactly what the CLI would use.
        .proxy(proxy)
        .max_idle_connections(0)
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_global(Some(ANSWER_TIMEOUT))
        .tls_config(
            TlsConfig::builder()
                .provider(TlsProvider::Rustls)
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .into())
}

fn ask(agent: &ureq::Agent, service: &Service) -> Result<Location, String> {
    let host = host(service.url);
    let mut response = agent
        .get(service.url)
        .call()
        .map_err(|err| format!("{host}: {err}"))?;
    if response.status() != 200 {
        return Err(format!("{host}: HTTP {}", response.status().as_u16()));
    }
    let text = response
        .body_mut()
        .with_config()
        .limit(ANSWER_LIMIT)
        .read_to_string()
        .map_err(|err| format!("{host}: {err}"))?;
    (service.parse)(&text).ok_or_else(|| format!("{host}: no country in the answer"))
}

fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split('/').next().unwrap_or(rest)
}

/// Cloudflare's trace: `ip=203.0.113.7`, `loc=FI` among other lines.
pub fn parse_trace(text: &str) -> Option<Location> {
    let mut country = None;
    let mut ip = None;
    for line in text.lines() {
        match line.split_once('=') {
            Some(("loc", value)) => country = normalize(value),
            Some(("ip", value)) if !value.trim().is_empty() => ip = Some(value.trim().to_string()),
            _ => {}
        }
    }
    Some(Location {
        country: country?,
        ip,
    })
}

/// country.is: `{"ip":"203.0.113.7","country":"FI"}`.
pub fn parse_country_is(text: &str) -> Option<Location> {
    let value: Value = serde_json::from_str(text).ok()?;
    Some(Location {
        country: normalize(value["country"].as_str()?)?,
        ip: value["ip"].as_str().map(str::to_string),
    })
}

/// A country code as Flux keeps it: two ASCII letters, upper case. Cloudflare's `XX` (not known)
/// and `T1` (Tor) aren't countries.
pub fn normalize(code: &str) -> Option<String> {
    let code = code.trim().to_ascii_uppercase();
    let letters = code.len() == 2 && code.bytes().all(|byte| byte.is_ascii_uppercase());
    (letters && code != "XX").then_some(code)
}

/// The codes of a list the user typed: "ru, by" → `["RU", "BY"]`. Commas, semicolons and spaces
/// separate them; repeats and what isn't a code ([`invalid_codes`]) are dropped.
pub fn parse_codes(text: &str) -> Vec<String> {
    let mut codes: Vec<String> = Vec::new();
    for code in words(text).filter_map(normalize) {
        if !codes.contains(&code) {
            codes.push(code);
        }
    }
    codes
}

/// What in a typed list isn't a country code, as typed.
pub fn invalid_codes(text: &str) -> Vec<String> {
    words(text)
        .filter(|word| normalize(word).is_none())
        .map(str::to_string)
        .collect()
}

fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .filter(|word| !word.is_empty())
}

/// The flag of a country: two regional indicator symbols ("FI" → 🇫🇮); empty for what isn't a code.
pub fn flag(code: &str) -> String {
    let Some(code) = normalize(code) else {
        return String::new();
    };
    code.bytes()
        .filter_map(|letter| char::from_u32(0x1F1E6 + u32::from(letter - b'A')))
        .collect()
}

/// The proxy the CLI's requests go through: the `env` of its user settings
/// (`$CLAUDE_CONFIG_DIR/settings.json`, `~/.claude/settings.json`), then the environment Flux gives
/// it; `None` — straight.
pub fn claude_proxy() -> Option<String> {
    let settings = settings_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .unwrap_or(Value::Null);
    proxy_from(&settings, |key| std::env::var(key).ok())
}

/// [`claude_proxy`] from the settings' JSON and an environment.
fn proxy_from(settings: &Value, env: impl Fn(&str) -> Option<String>) -> Option<String> {
    let set = |value: Option<String>| value.filter(|value| !value.trim().is_empty());
    PROXY_KEYS
        .iter()
        .find_map(|key| set(settings["env"][key].as_str().map(str::to_string)))
        .or_else(|| PROXY_KEYS.iter().find_map(|key| set(env(key))))
}

fn settings_path() -> Option<PathBuf> {
    let dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".claude")))?;
    Some(dir.join("settings.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_trace_gives_the_country_and_the_address() {
        let trace = "fl=123f45\nh=www.cloudflare.com\nip=203.0.113.7\nts=1761650000.1\n\
                     visit_scheme=https\nuag=Flux\ncolo=ARN\nhttp=http/2\nloc=FI\ntls=TLSv1.3\n";
        assert_eq!(
            parse_trace(trace),
            Some(Location {
                country: "FI".into(),
                ip: Some("203.0.113.7".into()),
            })
        );
        // Not known, Tor: no country.
        assert_eq!(parse_trace("ip=1.2.3.4\nloc=XX\n"), None);
        assert_eq!(parse_trace("ip=1.2.3.4\nloc=T1\n"), None);
        assert_eq!(parse_trace("<html>blocked</html>"), None);
    }

    #[test]
    fn country_is_answers_in_json() {
        assert_eq!(
            parse_country_is(r#"{"ip":"203.0.113.7","country":"ru"}"#),
            Some(Location {
                country: "RU".into(),
                ip: Some("203.0.113.7".into()),
            })
        );
        assert_eq!(parse_country_is(r#"{"error":"rate limited"}"#), None);
        assert_eq!(parse_country_is("not json"), None);
    }

    #[test]
    fn typed_lists_become_codes() {
        assert_eq!(parse_codes("ru, by;cn  RU"), ["RU", "BY", "CN"]);
        assert_eq!(parse_codes(""), Vec::<String>::new());
        assert_eq!(invalid_codes("ru, Russia, b1, by"), ["Russia", "b1"]);
    }

    #[test]
    fn flags_are_regional_indicators() {
        assert_eq!(flag("FI"), "🇫🇮");
        assert_eq!(flag("ru"), "🇷🇺");
        assert_eq!(flag("XX"), "");
        assert_eq!(flag("Finland"), "");
    }

    #[test]
    fn the_proxy_comes_from_claude_settings_first() {
        let env = |key: &str| (key == "HTTP_PROXY").then(|| "http://env:3128".to_string());
        let settings = json!({ "env": { "https_proxy": "http://settings:8080" } });
        assert_eq!(
            proxy_from(&settings, env),
            Some("http://settings:8080".into())
        );
        assert_eq!(proxy_from(&json!({}), env), Some("http://env:3128".into()));
        assert_eq!(
            proxy_from(&json!({ "env": { "HTTPS_PROXY": " " } }), |_| None),
            None
        );
    }

    /// The real services: `cargo test -p flux-claude live_detect -- --ignored --nocapture`.
    #[test]
    #[ignore = "asks the network"]
    fn live_detect() {
        let started = std::time::Instant::now();
        let location = detect(claude_proxy().as_deref());
        println!("{location:?} in {:?}", started.elapsed());
        assert!(location.is_ok());
    }

    #[test]
    fn hosts_for_errors() {
        assert_eq!(
            host("https://www.cloudflare.com/cdn-cgi/trace"),
            "www.cloudflare.com"
        );
        assert_eq!(host("https://api.country.is/"), "api.country.is");
    }
}
