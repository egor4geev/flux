//! User settings that persist between launches: `~/Library/Application Support/flux/settings.json`
//! (`FLUX_SETTINGS_FILE` — another file; in a scenario without it, nothing is read or written).
//! Edited in the Settings window ([`crate::settings_view`]).

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use gpui::{App, Global};
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Install a missing language server when a file needs it.
    pub auto_install_servers: bool,
    /// Where the settings are saved; `None` — kept in memory only.
    path: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auto_install_servers: true,
            path: None,
        }
    }
}

impl Global for Settings {}

pub fn init(cx: &mut App) {
    let path = store_path_for(
        std::env::var_os("FLUX_SETTINGS_FILE"),
        std::env::var_os("FLUX_SCENARIO").is_some(),
        std::env::var_os("HOME"),
    );
    let mut settings = path
        .as_deref()
        .and_then(|path| fs::read_to_string(path).ok())
        .map(|text| parse(&text))
        .unwrap_or_default();
    settings.path = path;
    cx.set_global(settings);
}

pub fn auto_install_servers(cx: &App) -> bool {
    cx.try_global::<Settings>()
        .is_none_or(|settings| settings.auto_install_servers)
}

pub fn set_auto_install_servers(on: bool, cx: &mut App) {
    let settings = cx.default_global::<Settings>();
    settings.auto_install_servers = on;
    if let Some(path) = &settings.path
        && let Err(err) = write(path, &to_json(settings))
    {
        eprintln!("flux: can't save settings to {}: {err}", path.display());
    }
}

fn store_path_for(
    explicit: Option<OsString>,
    scenario: bool,
    home: Option<OsString>,
) -> Option<PathBuf> {
    if let Some(file) = explicit.filter(|file| !file.is_empty()) {
        return Some(file.into());
    }
    if scenario {
        return None;
    }
    Some(PathBuf::from(home?).join("Library/Application Support/flux/settings.json"))
}

/// Unknown keys and broken files fall back to the defaults.
fn parse(text: &str) -> Settings {
    let value: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    let defaults = Settings::default();
    Settings {
        auto_install_servers: value["language_servers"]["auto_install"]
            .as_bool()
            .unwrap_or(defaults.auto_install_servers),
        ..defaults
    }
}

fn to_json(settings: &Settings) -> String {
    let value = json!({
        "language_servers": { "auto_install": settings.auto_install_servers },
    });
    serde_json::to_string_pretty(&value).unwrap_or_default() + "\n"
}

/// Through a temporary file and a `rename`: the file is never left half-written.
fn write(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let temp = path.with_extension("json.flux-tmp");
    fs::write(&temp, text)?;
    fs::rename(&temp, path).inspect_err(|_| {
        fs::remove_file(&temp).ok();
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_through_the_file() {
        let off = Settings {
            auto_install_servers: false,
            path: None,
        };
        assert_eq!(parse(&to_json(&off)), off);
        assert_eq!(parse(&to_json(&Settings::default())), Settings::default());
    }

    #[test]
    fn broken_or_partial_files_give_defaults() {
        assert_eq!(parse("not json"), Settings::default());
        assert_eq!(parse("{}"), Settings::default());
        assert_eq!(
            parse(r#"{"language_servers": {"auto_install": "no"}}"#),
            Settings::default()
        );
    }

    #[test]
    fn the_file_is_in_application_support_unless_given() {
        let home = Some(OsString::from("/Users/me"));
        assert_eq!(
            store_path_for(None, false, home.clone()),
            Some(PathBuf::from(
                "/Users/me/Library/Application Support/flux/settings.json"
            ))
        );
        assert_eq!(store_path_for(None, true, home.clone()), None);
        assert_eq!(
            store_path_for(Some("/tmp/s.json".into()), true, home),
            Some(PathBuf::from("/tmp/s.json"))
        );
    }
}
