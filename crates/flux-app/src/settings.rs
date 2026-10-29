//! User settings that persist between launches: `~/Library/Application Support/flux/settings.json`
//! (`FLUX_SETTINGS_FILE` — another file; in a scenario without it, nothing is read or written).
//! Edited in the Settings window ([`crate::settings_view`]) and, since part 10.5, in the file
//! itself: Flux reads it again when it changes ([`reload`], [`crate::settings_file`]). Keys Flux
//! doesn't know are kept as written; a file that isn't valid JSON is left alone — Flux keeps the
//! settings it has and doesn't write the file until it is fixed.

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
    /// Settings → Appearance (stage 8.3): the theme, the file icons, the interface language.
    pub appearance: AppearanceSettings,
    /// Look for updates of the installed plugins in the catalog when Flux starts
    /// (`plugins.check_updates`).
    pub check_plugin_updates: bool,
    /// Kinds of files whose plugin suggestion the user ignored ("*.rs", "Dockerfile";
    /// `plugins.ignored_suggestions`).
    pub ignored_suggestions: BTreeSet<String>,
    /// Settings → System → Projects (part 10.1): where another project opens, reopening the
    /// projects on startup.
    pub projects: ProjectSettings,
    /// How a Markdown file opens (part 10.4): the text, the text and its preview side by side, or
    /// the preview; the layout chosen last becomes the default (`markdown.layout`).
    pub markdown_layout: MarkdownLayout,
    /// Settings → Editor (part 10.5): soft wraps (`editor`).
    pub editor: EditorSettings,
    /// The welcome guide (part 10.7): What's New after an update (`welcome`).
    pub welcome: WelcomeSettings,
    /// Settings → Updates (part 10.8): checking for new versions of Flux and their channel
    /// (`updates`).
    pub updates: UpdateSettings,
    /// The user's settings of language servers (part 10.5), by server name, as written in
    /// `settings.json` (`lsp`): `settings`, `initialization_options`, the command and its arguments
    /// over what the language plugin brings ([`crate::lsp_settings`]).
    pub lsp: serde_json::Map<String, Value>,
    /// Keys of the file this Flux doesn't know (a newer Flux's, the user's own), kept as written
    /// (part 10.5): top-level keys, and keys inside the objects Flux reads (`appearance`…). The
    /// file is written from the struct, which would lose them.
    unknown: serde_json::Map<String, Value>,
    /// What is wrong with the file while it can't be read (broken JSON): Flux keeps the settings it
    /// has and doesn't write the file until it is fixed (part 10.5).
    error: Option<FileError>,
    /// Where the settings are saved; `None` — kept in memory only.
    path: Option<PathBuf>,
}

/// Why `settings.json` can't be read: the JSON parser's message and where it stopped (1-based; 0
/// — the file itself couldn't be read).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileError {
    pub message: String,
    pub line: usize,
    pub column: usize,
}

/// [`FileError::message`] of a file that is valid JSON, but not an object (the windows show it
/// translated).
pub const NOT_AN_OBJECT: &str = "The settings must be a JSON object";

/// What reading `settings.json` again found ([`reload`]).
pub enum Reload {
    /// The same settings: Flux's own write, a save without changes.
    Unchanged,
    /// Other settings, now in use: the previous ones, to see what changed; `fixed` — the file
    /// couldn't be read before.
    Changed { old: Box<Settings>, fixed: bool },
    /// The file can't be read: the settings stay as they were. `new` — not the error known before.
    Invalid { error: FileError, new: bool },
}

/// Settings → Editor (part 10.5), as JetBrains IDEs' Editor → General → Soft Wraps.
#[derive(Debug, Clone, PartialEq)]
pub struct EditorSettings {
    /// Files whose long lines wrap by default: masks of their names (`*.md`); the editor's own
    /// switch (⌃⌥W) overrides it for one editor.
    pub soft_wrap_files: Vec<String>,
    /// A wrapped part of a line starts at the line's indent ("Use original line's indent for wrapped
    /// parts").
    pub soft_wrap_indent: bool,
    /// Spaces added to that indent ("Add additional indent").
    pub soft_wrap_extra_indent: u32,
    /// The soft-wrap signs show on the caret's line only ("Only show soft-wrap indicators for the
    /// current line"); otherwise on every wrapped line.
    pub soft_wrap_signs_current_line_only: bool,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            soft_wrap_files: ["*.md", "*.txt", "*.rst", "*.adoc"]
                .map(String::from)
                .to_vec(),
            soft_wrap_indent: true,
            soft_wrap_extra_indent: 0,
            soft_wrap_signs_current_line_only: true,
        }
    }
}

/// The welcome guide (part 10.7, `crate::welcome`).
#[derive(Debug, Clone, PartialEq)]
pub struct WelcomeSettings {
    /// After an update to a new minor version, What's New shows once ("Show after updates").
    pub whats_new: bool,
}

impl Default for WelcomeSettings {
    fn default() -> Self {
        Self { whats_new: true }
    }
}

/// Updates of Flux (part 10.8, `crate::updates`).
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateSettings {
    /// Look for a new version a few seconds after Flux starts and then once a day
    /// (`updates.check_automatically`).
    pub check_automatically: bool,
    /// Stable releases, or betas too (`updates.channel`); a beta build starts on Beta.
    pub channel: flux_update::Channel,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self {
            check_automatically: true,
            channel: flux_update::Channel::of(&flux_update::current_version()),
        }
    }
}

/// Settings → Appearance (stage 8.3): themes and icons are chosen by name among those the
/// turned-on plugins bring; a name that is gone falls back to the default.
#[derive(Debug, Clone, PartialEq)]
pub struct AppearanceSettings {
    /// The chosen theme; `None` — Flux Night.
    pub theme: Option<String>,
    /// The theme follows macOS: [`AppearanceSettings::light_theme`] or
    /// [`AppearanceSettings::dark_theme`] (Sync with OS, as in JetBrains IDEs).
    pub sync_with_os: bool,
    /// The theme for macOS's light appearance; `None` — Flux Day.
    pub light_theme: Option<String>,
    /// The theme for macOS's dark appearance; `None` — Flux Night.
    pub dark_theme: Option<String>,
    /// The chosen set of file icons; `None` — Flux's own.
    pub icon_theme: Option<String>,
    /// The interface language.
    pub language: LanguageChoice,
    /// The look of the window (part 10.4): islands on glass, or flat panels edge to edge.
    pub ui_style: UiStyle,
    /// The interface scale (part 10.4), 1.0 — 100 %: the whole window, the code included, as Zoom
    /// IDE of JetBrains IDEs (`crate::ui_scale`).
    pub ui_scale: f32,
    /// Animations (2026-10-28, `crate::motion`): off, things simply appear. macOS's "Reduce
    /// motion" turns them off too.
    pub animations: bool,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            theme: None,
            sync_with_os: false,
            light_theme: None,
            dark_theme: None,
            icon_theme: None,
            language: LanguageChoice::default(),
            ui_style: UiStyle::default(),
            ui_scale: 1.,
            animations: true,
        }
    }
}

/// The look of the window (Settings → Appearance → Interface, part 10.4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum UiStyle {
    /// Islands on glass: rounded panels with gaps on the window's blurred frame (since the
    /// redesign of 2026-10-08).
    #[default]
    Islands,
    /// Flat panels edge to edge, divided by lines, on an opaque window (as before the redesign,
    /// and as the classic look of JetBrains IDEs).
    Flat,
}

impl UiStyle {
    pub fn key(self) -> &'static str {
        match self {
            UiStyle::Islands => "islands",
            UiStyle::Flat => "flat",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "islands" => Some(UiStyle::Islands),
            "flat" => Some(UiStyle::Flat),
            _ => None,
        }
    }
}

/// How a Markdown file opens (part 10.4), as the three buttons of JetBrains IDEs' Markdown editor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MarkdownLayout {
    /// The text only.
    Editor,
    /// The text and its preview side by side.
    #[default]
    Split,
    /// The preview only.
    Preview,
}

impl MarkdownLayout {
    pub fn key(self) -> &'static str {
        match self {
            MarkdownLayout::Editor => "editor",
            MarkdownLayout::Split => "split",
            MarkdownLayout::Preview => "preview",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "editor" => Some(MarkdownLayout::Editor),
            "split" => Some(MarkdownLayout::Split),
            "preview" => Some(MarkdownLayout::Preview),
            _ => None,
        }
    }
}

/// The interface language: the system's (the first of its preferred languages Flux speaks), or one
/// chosen. `FLUX_LANG` still wins (scenarios, screenshots).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LanguageChoice {
    #[default]
    System,
    English,
    Russian,
}

impl LanguageChoice {
    pub fn key(self) -> &'static str {
        match self {
            LanguageChoice::System => "system",
            LanguageChoice::English => "en",
            LanguageChoice::Russian => "ru",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "system" => Some(LanguageChoice::System),
            "en" => Some(LanguageChoice::English),
            "ru" => Some(LanguageChoice::Russian),
            _ => None,
        }
    }
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
    /// The sessions open when the project closed come back when it opens (part 9.2).
    pub restore_sessions: bool,
    /// Claude gets Flux's tools: problems of the language servers, navigation, the open files
    /// (the `flux` MCP server, part 9.2).
    pub flux_tools: bool,
    /// After an edit of Claude, the problems language servers find in the file go back to Claude.
    pub report_problems: bool,
    /// After a `!` command, Claude answers its output at once (a turn starts); off — the output
    /// only goes to Claude as context for the next message (part 10.2); ⌘↵ runs a command the
    /// other way. Saved as `answer_commands`: the earlier `respond_to_commands` was off by default
    /// and every saved file kept that `false`, so a new key lets the new default (on) take hold.
    pub answer_commands: bool,
    /// A macOS notification when a session finishes or waits for an answer while Flux isn't the
    /// active app (part 10.2).
    pub system_notifications: bool,
    /// The country check (part «Флаг страны», [`crate::region_guard`]): Claude is blocked while
    /// the IP address is in one of `blocked_countries` or its country can't be told.
    pub country_check: bool,
    /// ISO 3166-1 alpha-2 codes, upper case; Russia by default.
    pub blocked_countries: Vec<String>,
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
            restore_sessions: true,
            flux_tools: true,
            report_problems: true,
            answer_commands: true,
            system_notifications: true,
            country_check: true,
            blocked_countries: vec!["RU".to_string()],
        }
    }
}

/// Settings → System → Projects (part 10.1), as JetBrains IDEs have them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectSettings {
    /// Where a project opens when the window already has one (`projects.open_in`).
    pub open_in: OpenProjectIn,
    /// Flux started without a project (from the Dock, the Finder) opens the projects that were
    /// open when it quit (`projects.reopen`).
    pub reopen: bool,
}

impl Default for ProjectSettings {
    fn default() -> Self {
        Self {
            open_in: OpenProjectIn::default(),
            reopen: true,
        }
    }
}

/// Where another project opens: asked each time (with "Don't ask again", which remembers the
/// answer here), or always in this window or a new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OpenProjectIn {
    #[default]
    Ask,
    ThisWindow,
    NewWindow,
}

impl OpenProjectIn {
    pub fn key(self) -> &'static str {
        match self {
            OpenProjectIn::Ask => "ask",
            OpenProjectIn::ThisWindow => "this-window",
            OpenProjectIn::NewWindow => "new-window",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "ask" => Some(OpenProjectIn::Ask),
            "this-window" => Some(OpenProjectIn::ThisWindow),
            "new-window" => Some(OpenProjectIn::NewWindow),
            _ => None,
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
            appearance: AppearanceSettings::default(),
            check_plugin_updates: true,
            ignored_suggestions: BTreeSet::new(),
            projects: ProjectSettings::default(),
            markdown_layout: MarkdownLayout::default(),
            editor: EditorSettings::default(),
            welcome: WelcomeSettings::default(),
            updates: UpdateSettings::default(),
            lsp: serde_json::Map::new(),
            unknown: serde_json::Map::new(),
            error: None,
            path: None,
        }
    }
}

impl Global for Settings {}

/// Flux started without `settings.json` (part 10.7): the first launch — the welcome guide opens.
static FIRST_RUN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// This is the first launch: there was no `settings.json` when Flux started (a scenario without
/// `FLUX_SETTINGS_FILE` has no file at all — never a first launch).
pub fn first_run() -> bool {
    FIRST_RUN.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn init(cx: &mut App) {
    let path = store_path_for(
        std::env::var_os("FLUX_SETTINGS_FILE"),
        std::env::var_os("FLUX_SCENARIO").is_some(),
        std::env::var_os("HOME"),
    )
    // Watched and opened in a tab: the same path wherever the current directory is.
    .map(|path| std::path::absolute(&path).unwrap_or(path));
    // A broken file gives the defaults, but stays as it is (the windows tell about it).
    let mut settings = match path.as_deref().map(read_file) {
        Some(Err(error)) => Settings {
            error: Some(error),
            ..Settings::default()
        },
        Some(Ok(settings)) => settings,
        None => Settings::default(),
    };
    let first = path.as_deref().is_some_and(|path| !path.exists());
    FIRST_RUN.store(first, std::sync::atomic::Ordering::Relaxed);
    settings.path = path;
    cx.set_global(settings);
}

/// Reads `settings.json` again (part 10.5: it changed on disk — saved in a tab of Flux, edited by
/// another program, written by Flux itself). Valid settings replace those in use (observers of
/// the [`Settings`] global hear of it); a broken file leaves them and blocks writing.
pub fn reload(cx: &mut App) -> Reload {
    let Some(path) = cx.try_global::<Settings>().and_then(|s| s.path.clone()) else {
        return Reload::Unchanged;
    };
    let current = cx.global::<Settings>();
    match read_file(&path) {
        Err(error) => {
            let new = current.error.as_ref() != Some(&error);
            if new {
                cx.global_mut::<Settings>().error = Some(error.clone());
            }
            Reload::Invalid { error, new }
        }
        Ok(mut settings) => {
            settings.path = Some(path);
            if *current == settings {
                return Reload::Unchanged;
            }
            let old = std::mem::replace(cx.global_mut::<Settings>(), settings);
            let fixed = old.error.is_some();
            Reload::Changed {
                old: Box::new(old),
                fixed,
            }
        }
    }
}

/// The settings of the file at `path`: a missing file — the defaults.
fn read_file(path: &Path) -> Result<Settings, FileError> {
    match fs::read_to_string(path) {
        Ok(text) => parse_file(&text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(err) => Err(FileError {
            message: err.to_string(),
            line: 0,
            column: 0,
        }),
    }
}

/// Where the settings are saved (`settings.json`); `None` — kept in memory only (a scenario).
pub fn file_path(cx: &App) -> Option<PathBuf> {
    cx.try_global::<Settings>()
        .and_then(|settings| settings.path.clone())
}

/// What is wrong with `settings.json` while it can't be read (part 10.5).
pub fn file_error(cx: &App) -> Option<FileError> {
    cx.try_global::<Settings>()
        .and_then(|settings| settings.error.clone())
}

/// Writes the settings in use to the file now (it is about to be opened in a tab and doesn't exist
/// yet: nothing was ever changed).
pub fn save_file(cx: &App) {
    if let Some(settings) = cx.try_global::<Settings>() {
        save(settings);
    }
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

/// Settings → Appearance.
pub fn appearance(cx: &App) -> AppearanceSettings {
    cx.try_global::<Settings>()
        .map(|settings| settings.appearance.clone())
        .unwrap_or_default()
}

/// Changes the appearance settings and saves them. Applying them (the theme, the icons, the
/// language) is up to the caller.
pub fn update_appearance(cx: &mut App, change: impl FnOnce(&mut AppearanceSettings)) {
    let settings = cx.default_global::<Settings>();
    change(&mut settings.appearance);
    save(settings);
}

/// The look of the window (part 10.4).
pub fn ui_style(cx: &App) -> UiStyle {
    cx.try_global::<Settings>()
        .map(|settings| settings.appearance.ui_style)
        .unwrap_or_default()
}

/// The interface scale (part 10.4), 1.0 — 100 %.
pub fn ui_scale(cx: &App) -> f32 {
    cx.try_global::<Settings>()
        .map_or(1., |settings| settings.appearance.ui_scale)
}

/// Animations are on in the settings (`crate::motion::enabled` also asks macOS).
pub fn animations(cx: &App) -> bool {
    cx.try_global::<Settings>()
        .is_none_or(|settings| settings.appearance.animations)
}

/// How a Markdown file opens (part 10.4).
pub fn markdown_layout(cx: &App) -> MarkdownLayout {
    cx.try_global::<Settings>()
        .map(|settings| settings.markdown_layout)
        .unwrap_or_default()
}

pub fn set_markdown_layout(layout: MarkdownLayout, cx: &mut App) {
    let settings = cx.default_global::<Settings>();
    if settings.markdown_layout != layout {
        settings.markdown_layout = layout;
        save(settings);
    }
}

/// Whether Flux looks for plugin updates when it starts.
pub fn check_plugin_updates(cx: &App) -> bool {
    cx.try_global::<Settings>()
        .is_none_or(|settings| settings.check_plugin_updates)
}

pub fn set_check_plugin_updates(on: bool, cx: &mut App) {
    let settings = cx.default_global::<Settings>();
    settings.check_plugin_updates = on;
    save(settings);
}

/// Settings → System → Projects.
pub fn projects(cx: &App) -> ProjectSettings {
    cx.try_global::<Settings>()
        .map(|settings| settings.projects)
        .unwrap_or_default()
}

/// Changes the project settings and saves them.
pub fn update_projects(cx: &mut App, change: impl FnOnce(&mut ProjectSettings)) {
    let settings = cx.default_global::<Settings>();
    change(&mut settings.projects);
    save(settings);
}

/// Settings → Editor (part 10.5).
pub fn editor(cx: &App) -> EditorSettings {
    cx.try_global::<Settings>()
        .map(|settings| settings.editor.clone())
        .unwrap_or_default()
}

/// Changes the editor settings and saves them.
pub fn update_editor(cx: &mut App, change: impl FnOnce(&mut EditorSettings)) {
    let settings = cx.default_global::<Settings>();
    change(&mut settings.editor);
    save(settings);
}

/// The welcome guide's settings (part 10.7).
pub fn welcome(cx: &App) -> WelcomeSettings {
    cx.try_global::<Settings>()
        .map(|settings| settings.welcome.clone())
        .unwrap_or_default()
}

pub fn update_welcome(cx: &mut App, change: impl FnOnce(&mut WelcomeSettings)) {
    let settings = cx.default_global::<Settings>();
    change(&mut settings.welcome);
    save(settings);
}

/// Settings → Updates (part 10.8).
pub fn updates(cx: &App) -> UpdateSettings {
    cx.try_global::<Settings>()
        .map(|settings| settings.updates.clone())
        .unwrap_or_default()
}

pub fn update_updates(cx: &mut App, change: impl FnOnce(&mut UpdateSettings)) {
    let settings = cx.default_global::<Settings>();
    change(&mut settings.updates);
    save(settings);
}

/// The user's settings of the language server `name` (part 10.5), as written under `lsp` in
/// `settings.json`; `None` — the language plugin's.
pub fn lsp_server(name: &str, cx: &App) -> Option<Value> {
    cx.try_global::<Settings>()
        .and_then(|settings| settings.lsp.get(name).cloned())
}

/// The user's settings of every language server (`lsp`), by server name.
pub fn lsp(cx: &App) -> serde_json::Map<String, Value> {
    cx.try_global::<Settings>()
        .map(|settings| settings.lsp.clone())
        .unwrap_or_default()
}

/// Changes the user's settings of language servers and saves them; the window's servers follow
/// ([`crate::lsp_settings`]).
pub fn update_lsp(cx: &mut App, change: impl FnOnce(&mut serde_json::Map<String, Value>)) {
    let settings = cx.default_global::<Settings>();
    change(&mut settings.lsp);
    save(settings);
}

/// Whether the user ignored plugin suggestions for this kind of file ("*.rs", "Dockerfile").
pub fn suggestion_ignored(kind: &str, cx: &App) -> bool {
    cx.try_global::<Settings>()
        .is_some_and(|settings| settings.ignored_suggestions.contains(kind))
}

/// Ignores plugin suggestions for a kind of file ("Ignore extension").
pub fn ignore_suggestion(kind: &str, cx: &mut App) {
    let settings = cx.default_global::<Settings>();
    if settings.ignored_suggestions.insert(kind.to_string()) {
        save(settings);
    }
}

fn save(settings: &Settings) {
    // A file the user broke stays as it is until they fix it (part 10.5).
    if settings.error.is_some() {
        return;
    }
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

/// Broken files fall back to the defaults.
#[cfg(test)]
fn parse(text: &str) -> Settings {
    parse_value(&serde_json::from_str(text).unwrap_or(Value::Null))
}

/// The settings `text` holds (part 10.5): a JSON object; an empty file is the defaults. Anything
/// else is an error with the place the parser stopped at.
pub(crate) fn parse_file(text: &str) -> Result<Settings, FileError> {
    if text.trim().is_empty() {
        return Ok(Settings::default());
    }
    match serde_json::from_str::<Value>(text) {
        Ok(value) if value.is_object() => Ok(parse_value(&value)),
        Ok(_) => Err(FileError {
            message: NOT_AN_OBJECT.to_string(),
            line: 1,
            column: 1,
        }),
        Err(err) => {
            // "expected `,` or `}` at line 3 column 5" — the place goes apart.
            let message = err.to_string();
            let message = match message.rfind(" at line ") {
                Some(at) => message[..at].to_string(),
                None => message,
            };
            Err(FileError {
                message,
                line: err.line(),
                column: err.column(),
            })
        }
    }
}

/// The settings of a parsed file: values of the wrong type fall back to the defaults; keys Flux
/// doesn't know are kept ([`Settings::unknown`]).
fn parse_value(value: &Value) -> Settings {
    let defaults = Settings::default();
    let mut settings = Settings {
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
        appearance: parse_appearance(&value["appearance"]),
        check_plugin_updates: value["plugins"]["check_updates"]
            .as_bool()
            .unwrap_or(defaults.check_plugin_updates),
        ignored_suggestions: value["plugins"]["ignored_suggestions"]
            .as_array()
            .map(|kinds| {
                kinds
                    .iter()
                    .filter_map(|kind| Some(kind.as_str()?.to_string()))
                    .collect()
            })
            .unwrap_or_default(),
        projects: ProjectSettings {
            open_in: value["projects"]["open_in"]
                .as_str()
                .and_then(OpenProjectIn::from_key)
                .unwrap_or(defaults.projects.open_in),
            reopen: value["projects"]["reopen"]
                .as_bool()
                .unwrap_or(defaults.projects.reopen),
        },
        markdown_layout: value["markdown"]["layout"]
            .as_str()
            .and_then(MarkdownLayout::from_key)
            .unwrap_or_default(),
        editor: parse_editor(&value["editor"]),
        welcome: WelcomeSettings {
            whats_new: value["welcome"]["whats_new"]
                .as_bool()
                .unwrap_or(defaults.welcome.whats_new),
        },
        updates: UpdateSettings {
            check_automatically: value["updates"]["check_automatically"]
                .as_bool()
                .unwrap_or(defaults.updates.check_automatically),
            channel: value["updates"]["channel"]
                .as_str()
                .and_then(flux_update::Channel::from_key)
                .unwrap_or(defaults.updates.channel),
        },
        lsp: value["lsp"].as_object().cloned().unwrap_or_default(),
        ..defaults
    };
    settings.unknown = unknown_keys(value, &known_json(&settings));
    settings
}

/// The keys of `raw` that the struct doesn't write (`known`): top-level ones, and those inside the
/// objects both have (`appearance.something_new`).
fn unknown_keys(raw: &Value, known: &Value) -> serde_json::Map<String, Value> {
    let (Some(raw), Some(known)) = (raw.as_object(), known.as_object()) else {
        return serde_json::Map::new();
    };
    let mut unknown = serde_json::Map::new();
    for (key, value) in raw {
        match (known.get(key), value) {
            (None, _) => {
                unknown.insert(key.clone(), value.clone());
            }
            (Some(Value::Object(known)), Value::Object(value)) => {
                let inner: serde_json::Map<String, Value> = value
                    .iter()
                    .filter(|(key, _)| !known.contains_key(*key))
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect();
                if !inner.is_empty() {
                    unknown.insert(key.clone(), Value::Object(inner));
                }
            }
            _ => {}
        }
    }
    unknown
}

/// Puts the kept unknown keys back among those the struct writes.
fn restore_unknown(value: &mut Value, unknown: &serde_json::Map<String, Value>) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    for (key, kept) in unknown {
        match (object.get_mut(key), kept) {
            (None, _) => {
                object.insert(key.clone(), kept.clone());
            }
            (Some(Value::Object(inner)), Value::Object(kept)) => {
                for (key, value) in kept {
                    inner.entry(key.clone()).or_insert_with(|| value.clone());
                }
            }
            _ => {}
        }
    }
}

fn parse_editor(value: &Value) -> EditorSettings {
    let defaults = EditorSettings::default();
    EditorSettings {
        soft_wrap_files: value["soft_wrap_files"]
            .as_array()
            .map(|masks| {
                masks
                    .iter()
                    .filter_map(|mask| Some(mask.as_str()?.to_string()))
                    .collect()
            })
            .unwrap_or(defaults.soft_wrap_files),
        soft_wrap_indent: value["soft_wrap_indent"]
            .as_bool()
            .unwrap_or(defaults.soft_wrap_indent),
        soft_wrap_extra_indent: value["soft_wrap_extra_indent"]
            .as_u64()
            .map_or(defaults.soft_wrap_extra_indent, |spaces| {
                spaces.min(16) as u32
            }),
        soft_wrap_signs_current_line_only: value["soft_wrap_signs_current_line_only"]
            .as_bool()
            .unwrap_or(defaults.soft_wrap_signs_current_line_only),
    }
}

fn editor_json(editor: &EditorSettings) -> Value {
    json!({
        "soft_wrap_files": editor.soft_wrap_files,
        "soft_wrap_indent": editor.soft_wrap_indent,
        "soft_wrap_extra_indent": editor.soft_wrap_extra_indent,
        "soft_wrap_signs_current_line_only": editor.soft_wrap_signs_current_line_only,
    })
}

fn parse_appearance(value: &Value) -> AppearanceSettings {
    let text = |key: &str| {
        value[key]
            .as_str()
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    };
    AppearanceSettings {
        theme: text("theme"),
        sync_with_os: value["sync_with_os"].as_bool().unwrap_or(false),
        light_theme: text("light_theme"),
        dark_theme: text("dark_theme"),
        icon_theme: text("icon_theme"),
        language: value["language"]
            .as_str()
            .and_then(LanguageChoice::from_key)
            .unwrap_or_default(),
        ui_style: value["ui_style"]
            .as_str()
            .and_then(UiStyle::from_key)
            .unwrap_or_default(),
        ui_scale: value["ui_scale"]
            .as_f64()
            .map(|scale| scale as f32)
            .filter(|scale| scale.is_finite())
            .map_or(1., crate::ui_scale::clamp),
        animations: value["animations"].as_bool().unwrap_or(true),
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
        restore_sessions: value["restore_sessions"]
            .as_bool()
            .unwrap_or(defaults.restore_sessions),
        flux_tools: value["flux_tools"].as_bool().unwrap_or(defaults.flux_tools),
        report_problems: value["report_problems"]
            .as_bool()
            .unwrap_or(defaults.report_problems),
        answer_commands: value["answer_commands"]
            .as_bool()
            .unwrap_or(defaults.answer_commands),
        system_notifications: value["system_notifications"]
            .as_bool()
            .unwrap_or(defaults.system_notifications),
        country_check: value["country_check"]
            .as_bool()
            .unwrap_or(defaults.country_check),
        // An empty list stays empty (then only a country that can't be told blocks Claude).
        blocked_countries: value["blocked_countries"]
            .as_array()
            .map(|codes| {
                codes
                    .iter()
                    .filter_map(|code| flux_claude::region::normalize(code.as_str()?))
                    .collect()
            })
            .unwrap_or(defaults.blocked_countries),
    }
}

fn to_json(settings: &Settings) -> String {
    let mut value = known_json(settings);
    restore_unknown(&mut value, &settings.unknown);
    serde_json::to_string_pretty(&value).unwrap_or_default() + "\n"
}

/// What the struct writes: every key Flux knows.
fn known_json(settings: &Settings) -> Value {
    json!({
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
            "check_updates": settings.check_plugin_updates,
            "ignored_suggestions": settings.ignored_suggestions.iter().collect::<Vec<_>>(),
        },
        "appearance": {
            "theme": settings.appearance.theme,
            "sync_with_os": settings.appearance.sync_with_os,
            "light_theme": settings.appearance.light_theme,
            "dark_theme": settings.appearance.dark_theme,
            "icon_theme": settings.appearance.icon_theme,
            "language": settings.appearance.language.key(),
            "ui_style": settings.appearance.ui_style.key(),
            // 0.9, not the f32's 0.8999999761581421.
            "ui_scale": (f64::from(settings.appearance.ui_scale) * 100.).round() / 100.,
            "animations": settings.appearance.animations,
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
            "restore_sessions": settings.claude.restore_sessions,
            "flux_tools": settings.claude.flux_tools,
            "report_problems": settings.claude.report_problems,
            "answer_commands": settings.claude.answer_commands,
            "system_notifications": settings.claude.system_notifications,
            "country_check": settings.claude.country_check,
            "blocked_countries": settings.claude.blocked_countries,
        },
        "projects": {
            "open_in": settings.projects.open_in.key(),
            "reopen": settings.projects.reopen,
        },
        "markdown": { "layout": settings.markdown_layout.key() },
        "editor": editor_json(&settings.editor),
        "welcome": { "whats_new": settings.welcome.whats_new },
        "updates": {
            "check_automatically": settings.updates.check_automatically,
            "channel": settings.updates.channel.key(),
        },
        "lsp": settings.lsp,
    })
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
                restore_sessions: false,
                flux_tools: false,
                report_problems: false,
                answer_commands: false,
                system_notifications: false,
                country_check: false,
                blocked_countries: vec!["RU".to_string(), "BY".to_string()],
            },
            appearance: AppearanceSettings {
                theme: Some("Flux Day".to_string()),
                sync_with_os: true,
                light_theme: Some("Solarized Light".to_string()),
                dark_theme: Some("Flux Night".to_string()),
                icon_theme: Some("Flux Icons".to_string()),
                language: LanguageChoice::Russian,
                ui_style: UiStyle::Flat,
                ui_scale: 1.25,
                animations: false,
            },
            check_plugin_updates: false,
            ignored_suggestions: BTreeSet::from(["*.rs".to_string()]),
            projects: ProjectSettings {
                open_in: OpenProjectIn::NewWindow,
                reopen: false,
            },
            markdown_layout: MarkdownLayout::Preview,
            editor: EditorSettings {
                soft_wrap_files: vec!["*.md".to_string(), "COMMIT_EDITMSG".to_string()],
                soft_wrap_indent: false,
                soft_wrap_extra_indent: 2,
                soft_wrap_signs_current_line_only: false,
            },
            welcome: WelcomeSettings { whats_new: false },
            updates: UpdateSettings {
                check_automatically: false,
                channel: flux_update::Channel::Beta,
            },
            lsp: serde_json::Map::from_iter([(
                "rust-analyzer".to_string(),
                json!({ "settings": { "rust-analyzer": { "check": { "command": "clippy" } } } }),
            )]),
            unknown: serde_json::Map::new(),
            error: None,
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

    #[test]
    fn keys_flux_doesnt_know_are_kept() {
        let text = r#"{
            "from_the_future": { "x": [1, 2] },
            "appearance": { "theme": "Flux Day", "glow": 0.5 },
            "claude": { "later": true },
            "lsp": { "gopls": { "settings": { "gopls": { "staticcheck": true } } } },
            "notifications": { "groups": { "git": "log", "weird": "loud" } }
        }"#;
        let mut settings = parse_file(text).unwrap();
        assert_eq!(settings.appearance.theme.as_deref(), Some("Flux Day"));
        assert_eq!(
            settings.unknown,
            serde_json::Map::from_iter([
                ("from_the_future".to_string(), json!({ "x": [1, 2] })),
                ("appearance".to_string(), json!({ "glow": 0.5 })),
                ("claude".to_string(), json!({ "later": true })),
            ])
        );
        // Flux changes what it knows; the rest is written back as it was.
        settings.appearance.theme = Some("Flux Night".to_string());
        let written: Value = serde_json::from_str(&to_json(&settings)).unwrap();
        assert_eq!(written["from_the_future"], json!({ "x": [1, 2] }));
        assert_eq!(written["appearance"]["glow"], json!(0.5));
        assert_eq!(written["appearance"]["theme"], json!("Flux Night"));
        assert_eq!(written["claude"]["later"], json!(true));
        assert_eq!(written["claude"]["enabled"], json!(true));
        assert_eq!(
            written["lsp"],
            json!({ "gopls": { "settings": { "gopls": { "staticcheck": true } } } })
        );
        // A broken entry of a list Flux owns is not "unknown": it is dropped as before.
        assert_eq!(written["notifications"]["groups"], json!({ "git": "log" }));
        // And the round trip is stable.
        assert_eq!(parse_file(&to_json(&settings)).unwrap(), settings);
    }

    #[test]
    fn a_broken_file_is_an_error_with_its_place() {
        let error =
            parse_file("{\n  \"git\": {\n    \"update_method\": \"merge\"\n  \n}").unwrap_err();
        assert_eq!(error.line, 5);
        assert!(error.message.contains("EOF"), "{}", error.message);
        assert!(!error.message.contains(" at line "), "{}", error.message);
        let error = parse_file("{ \"a\": 1, }").unwrap_err();
        assert_eq!((error.line, error.column), (1, 11));
        assert_eq!(
            parse_file("[1, 2]").unwrap_err().message,
            NOT_AN_OBJECT.to_string()
        );
        // An empty file is the defaults (the user cleared it).
        assert_eq!(parse_file("").unwrap(), Settings::default());
        assert_eq!(parse_file(" \n").unwrap(), Settings::default());
    }

    #[test]
    fn a_broken_file_is_not_overwritten() {
        let dir = std::env::temp_dir().join(format!("flux-settings-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        fs::write(&path, "{ broken").unwrap();
        let error = read_file(&path).unwrap_err();
        let settings = Settings {
            error: Some(error),
            path: Some(path.clone()),
            ..Settings::default()
        };
        save(&settings);
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ broken");
        // Fixed, it is written again.
        let settings = Settings {
            path: Some(path.clone()),
            ..Settings::default()
        };
        save(&settings);
        assert_eq!(read_file(&path).unwrap(), Settings::default());
        // A missing file is the defaults.
        assert_eq!(
            read_file(&dir.join("none.json")).unwrap(),
            Settings::default()
        );
        fs::remove_dir_all(&dir).ok();
    }
}
