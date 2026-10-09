//! User settings that persist between launches: `~/Library/Application Support/flux/settings.json`
//! (`FLUX_SETTINGS_FILE` — another file; in a scenario without it, nothing is read or written).
//! Edited in the Settings window ([`crate::settings_view`]).

use std::collections::{BTreeMap, BTreeSet};
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
    /// Plugins the user turned off, by id (`plugins.disabled`); the rest are on.
    pub disabled_plugins: BTreeSet<String>,
    /// Folders of plugins under development (`plugins.dev`).
    pub dev_plugins: Vec<PathBuf>,
    /// The values of plugins' settings the user changed, by plugin id (`plugins.settings`); the
    /// rest have the manifest's defaults.
    pub plugin_values: BTreeMap<String, serde_json::Map<String, Value>>,
    /// Claude Code (`claude`): the chat's defaults.
    pub claude: ClaudeSettings,
    /// Where the settings are saved; `None` — kept in memory only.
    path: Option<PathBuf>,
}

/// Claude Code (stage 9): the chat's defaults, edited in Settings → Claude Code. A new session
/// starts with them; the chat's own pickers change only that session.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeSettings {
    /// Claude Code is on: the launchpad icon, ⌘Esc, the commands.
    pub enabled: bool,
    /// The `claude` executable the user chose; `None` — found on the PATH and in the usual places.
    pub path: Option<PathBuf>,
    /// The model of new sessions: an alias ("opus", "sonnet") or a full name; `None` — the CLI's
    /// default.
    pub model: Option<String>,
    /// The effort of new sessions ("low" … "max"); `None` — the CLI's default.
    pub effort: Option<String>,
    /// The permission mode of new sessions ("default", "acceptEdits", "plan"…); `None` — the CLI's
    /// default.
    pub permission_mode: Option<String>,
    /// An edit Claude asks about opens in a diff tab as well, not only as a card in the chat.
    pub diff_tabs: bool,
    /// The editor's selection goes along with a message (a chip above the field).
    pub share_selection: bool,
    /// The subscription's limits (5 hours, 7 days) in the status bar.
    pub show_limits: bool,
    /// More arguments for `claude`, as the user typed them.
    pub extra_args: Vec<String>,
}

impl Default for ClaudeSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            path: None,
            model: None,
            effort: None,
            permission_mode: None,
            diff_tabs: true,
            share_selection: true,
            show_limits: true,
            extra_args: Vec::new(),
        }
    }
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
            disabled_plugins: BTreeSet::new(),
            dev_plugins: Vec::new(),
            plugin_values: BTreeMap::new(),
            claude: ClaudeSettings::default(),
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

pub fn plugin_enabled(id: &str, cx: &App) -> bool {
    cx.try_global::<Settings>()
        .is_none_or(|settings| !settings.disabled_plugins.contains(id))
}

pub fn set_plugin_enabled(id: &str, on: bool, cx: &mut App) {
    let settings = cx.default_global::<Settings>();
    if on {
        settings.disabled_plugins.remove(id);
    } else {
        settings.disabled_plugins.insert(id.to_string());
    }
    save(settings);
}

pub fn dev_plugins(cx: &App) -> Vec<PathBuf> {
    cx.try_global::<Settings>()
        .map(|settings| settings.dev_plugins.clone())
        .unwrap_or_default()
}

pub fn add_dev_plugin(dir: PathBuf, cx: &mut App) {
    let settings = cx.default_global::<Settings>();
    if !settings.dev_plugins.contains(&dir) {
        settings.dev_plugins.push(dir);
        save(settings);
    }
}

pub fn remove_dev_plugin(dir: &Path, cx: &mut App) {
    let settings = cx.default_global::<Settings>();
    settings.dev_plugins.retain(|known| known != dir);
    save(settings);
}

/// The values the user gave a plugin's settings, by key (the rest are the manifest's defaults).
pub fn plugin_values(id: &str, cx: &App) -> serde_json::Map<String, Value> {
    cx.try_global::<Settings>()
        .and_then(|settings| settings.plugin_values.get(id).cloned())
        .unwrap_or_default()
}

/// Sets a plugin's setting; `None` — back to the manifest's default.
pub fn set_plugin_value(id: &str, key: &str, value: Option<Value>, cx: &mut App) {
    let settings = cx.default_global::<Settings>();
    let values = settings.plugin_values.entry(id.to_string()).or_default();
    match value {
        Some(value) => {
            values.insert(key.to_string(), value);
        }
        None => {
            values.remove(key);
        }
    }
    if values.is_empty() {
        settings.plugin_values.remove(id);
    }
    save(settings);
}

/// Claude Code's settings.
pub fn claude(cx: &App) -> ClaudeSettings {
    cx.try_global::<Settings>()
        .map(|settings| settings.claude.clone())
        .unwrap_or_default()
}

/// Changes Claude Code's settings and saves them.
pub fn update_claude(cx: &mut App, change: impl FnOnce(&mut ClaudeSettings)) {
    let settings = cx.default_global::<Settings>();
    change(&mut settings.claude);
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
        disabled_plugins: value["plugins"]["disabled"]
            .as_array()
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| Some(id.as_str()?.to_string()))
                    .collect()
            })
            .unwrap_or_default(),
        dev_plugins: value["plugins"]["dev"]
            .as_array()
            .map(|dirs| {
                dirs.iter()
                    .filter_map(|dir| Some(PathBuf::from(dir.as_str()?)))
                    .collect()
            })
            .unwrap_or_default(),
        plugin_values: value["plugins"]["settings"]
            .as_object()
            .map(|plugins| {
                plugins
                    .iter()
                    .filter_map(|(id, values)| Some((id.clone(), values.as_object()?.clone())))
                    .collect()
            })
            .unwrap_or_default(),
        claude: parse_claude(&value["claude"]),
        ..defaults
    }
}

fn parse_claude(value: &Value) -> ClaudeSettings {
    let defaults = ClaudeSettings::default();
    let text = |key: &str| {
        value[key]
            .as_str()
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    };
    ClaudeSettings {
        enabled: value["enabled"].as_bool().unwrap_or(defaults.enabled),
        path: text("path").map(PathBuf::from),
        model: text("model"),
        effort: text("effort"),
        permission_mode: text("permission_mode"),
        diff_tabs: value["diff_tabs"].as_bool().unwrap_or(defaults.diff_tabs),
        share_selection: value["share_selection"]
            .as_bool()
            .unwrap_or(defaults.share_selection),
        show_limits: value["show_limits"]
            .as_bool()
            .unwrap_or(defaults.show_limits),
        extra_args: value["extra_args"]
            .as_array()
            .map(|args| {
                args.iter()
                    .filter_map(|arg| Some(arg.as_str()?.to_string()))
                    .collect()
            })
            .unwrap_or_default(),
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
        "plugins": {
            "disabled": settings.disabled_plugins.iter().collect::<Vec<_>>(),
            "dev": settings
                .dev_plugins
                .iter()
                .map(|dir| dir.to_string_lossy())
                .collect::<Vec<_>>(),
            "settings": settings.plugin_values,
        },
        "claude": {
            "enabled": settings.claude.enabled,
            "path": settings.claude.path.as_ref().map(|path| path.to_string_lossy()),
            "model": settings.claude.model,
            "effort": settings.claude.effort,
            "permission_mode": settings.claude.permission_mode,
            "diff_tabs": settings.claude.diff_tabs,
            "share_selection": settings.claude.share_selection,
            "show_limits": settings.claude.show_limits,
            "extra_args": settings.claude.extra_args,
        },
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
            disabled_plugins: BTreeSet::from(["flux.todo".to_string()]),
            dev_plugins: vec![PathBuf::from("/Users/me/dev/hello")],
            plugin_values: BTreeMap::from([(
                "flux.todo".to_string(),
                serde_json::Map::from_iter([("patterns".to_string(), json!(["TODO"]))]),
            )]),
            claude: ClaudeSettings {
                enabled: false,
                path: Some(PathBuf::from("/opt/homebrew/bin/claude")),
                model: Some("opus".to_string()),
                effort: Some("high".to_string()),
                permission_mode: Some("acceptEdits".to_string()),
                diff_tabs: false,
                share_selection: false,
                show_limits: false,
                extra_args: vec!["--verbose".to_string()],
            },
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
