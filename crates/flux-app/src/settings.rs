//! User settings that persist between launches: `~/Library/Application Support/flux/settings.json`
//! (`FLUX_SETTINGS_FILE` — another file; in a scenario without it, nothing is read or written).
//! Edited in the Settings window ([`crate::settings_view`]).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use gpui::{App, Global};
use serde_json::{Value, json};

use crate::notification_center::Display;

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Install a missing language server when a file needs it.
    pub auto_install_servers: bool,
    /// How Update Project brings the incoming commits into the current branch.
    pub update_method: UpdatePreference,
    /// Notifications: cards off for every group (the journal still gets everything).
    pub do_not_disturb: bool,
    /// How each notification group is shown, by group key ("git", "plugin:claude"); a group not
    /// here has its default.
    pub notification_displays: BTreeMap<String, Display>,
    /// Ask before terminating a command that runs in a terminal being closed ("Don't ask again"
    /// turns it off).
    pub confirm_terminate: bool,
    /// Where the settings are saved; `None` — kept in memory only.
    path: Option<PathBuf>,
}

/// Update Project's method: asked the first time (as JetBrains IDEs do), then remembered when the
/// user says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UpdatePreference {
    #[default]
    Ask,
    Merge,
    Rebase,
}

impl UpdatePreference {
    fn key(self) -> &'static str {
        match self {
            UpdatePreference::Ask => "ask",
            UpdatePreference::Merge => "merge",
            UpdatePreference::Rebase => "rebase",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        match key {
            "ask" => Some(UpdatePreference::Ask),
            "merge" => Some(UpdatePreference::Merge),
            "rebase" => Some(UpdatePreference::Rebase),
            _ => None,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auto_install_servers: true,
            update_method: UpdatePreference::default(),
            do_not_disturb: false,
            notification_displays: BTreeMap::new(),
            confirm_terminate: true,
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
    save(settings);
}

pub fn update_method(cx: &App) -> UpdatePreference {
    cx.try_global::<Settings>()
        .map_or(UpdatePreference::default(), |settings| {
            settings.update_method
        })
}

pub fn set_update_method(method: UpdatePreference, cx: &mut App) {
    let settings = cx.default_global::<Settings>();
    settings.update_method = method;
    save(settings);
}

pub fn do_not_disturb(cx: &App) -> bool {
    cx.try_global::<Settings>()
        .is_some_and(|settings| settings.do_not_disturb)
}

pub fn set_do_not_disturb(on: bool, cx: &mut App) {
    let settings = cx.default_global::<Settings>();
    settings.do_not_disturb = on;
    save(settings);
}

/// How the notifications of the group with `key` are shown, if the user chose it.
pub fn notification_display(key: &str, cx: &App) -> Option<Display> {
    cx.try_global::<Settings>()
        .and_then(|settings| settings.notification_displays.get(key).copied())
}

pub fn set_notification_display(key: &str, display: Display, cx: &mut App) {
    let settings = cx.default_global::<Settings>();
    settings
        .notification_displays
        .insert(key.to_string(), display);
    save(settings);
}

pub fn confirm_terminate(cx: &App) -> bool {
    cx.try_global::<Settings>()
        .is_none_or(|settings| settings.confirm_terminate)
}

pub fn set_confirm_terminate(on: bool, cx: &mut App) {
    let settings = cx.default_global::<Settings>();
    settings.confirm_terminate = on;
    save(settings);
}

fn save(settings: &Settings) {
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
        update_method: value["git"]["update_method"]
            .as_str()
            .and_then(UpdatePreference::from_key)
            .unwrap_or(defaults.update_method),
        do_not_disturb: value["notifications"]["do_not_disturb"]
            .as_bool()
            .unwrap_or(defaults.do_not_disturb),
        notification_displays: value["notifications"]["groups"]
            .as_object()
            .map(|groups| {
                groups
                    .iter()
                    .filter_map(|(key, display)| {
                        Some((key.clone(), Display::from_key(display.as_str()?)?))
                    })
                    .collect()
            })
            .unwrap_or_default(),
        confirm_terminate: value["terminal"]["confirm_terminate"]
            .as_bool()
            .unwrap_or(defaults.confirm_terminate),
        ..defaults
    }
}

fn to_json(settings: &Settings) -> String {
    let value = json!({
        "language_servers": { "auto_install": settings.auto_install_servers },
        "git": { "update_method": settings.update_method.key() },
        "notifications": {
            "do_not_disturb": settings.do_not_disturb,
            "groups": settings
                .notification_displays
                .iter()
                .map(|(key, display)| (key.clone(), Value::from(display.key())))
                .collect::<serde_json::Map<_, _>>(),
        },
        "terminal": { "confirm_terminate": settings.confirm_terminate },
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
            update_method: UpdatePreference::Rebase,
            do_not_disturb: true,
            notification_displays: BTreeMap::from([
                ("git".to_string(), Display::StickyBalloon),
                ("plugin:claude".to_string(), Display::Hidden),
            ]),
            confirm_terminate: false,
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
        // An unknown display or a wrong type drops only that entry.
        let parsed = parse(
            r#"{"notifications": {"do_not_disturb": 1, "groups": {"git": "log", "files": "loud", "terminal": 3}}}"#,
        );
        assert!(!parsed.do_not_disturb);
        assert_eq!(
            parsed.notification_displays,
            BTreeMap::from([("git".to_string(), Display::LogOnly)])
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
