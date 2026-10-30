//! Settings (⌘, or the gear at the bottom of the launchpad): an overlay window with sections on the
//! left.
//! - System Settings (part 10.1): reopening the projects on startup, and where another project
//!   opens — a new window, this one, or ask (JetBrains: Appearance & Behavior → System Settings).
//! - Updates (part 10.8): this version and its channel, Check Now, checking by itself, the channel
//!   ([`crate::update_settings`]).
//! - Editor (part 10.5): soft wraps ([`crate::editor_settings`]).
//! - Language Servers: automatic installation on or off, and every server Flux knows with where it
//!   comes from; those Flux installed can be updated and deleted, missing ones installed. Each can
//!   be turned off, and "Configure…" opens its key in `settings.json` (part 10.5,
//!   [`crate::lsp_settings`]).
//! - Version Control: how Update Project (⌘T) brings incoming commits into the current branch —
//!   merge, rebase, or ask every time (the question's "Don't show again" sets it).
//! - Notifications: Do Not Disturb, and for every group (plugins' too) how its notifications show —
//!   a card, a sticky card, the journal only, or nothing (JetBrains: Settings → Notifications).
//! - Plugins: the plugin manager ([`crate::plugin_manager`]); under it, a page of every plugin that
//!   is on and has settings ([`crate::plugin_settings`]).
//! - Claude Code: on or off, the country check (part «Флаг страны»: the switch, the blocked
//!   countries, where the network sees this Mac now — [`crate::region_guard`]), the `claude`
//!   executable (found or chosen), its version and account, the defaults of new sessions, the
//!   chat's switches, more arguments for the CLI.
//! - About: the version with the commit it is built from, macOS and the computer, the license, the
//!   developer, the source code; Report a Bug… and Copy — the facts for a report, as JetBrains' Copy
//!   and Close (part 10.6, [`crate::feedback`]). «About Flux» in the app menu ([`About`]) opens
//!   Settings on this section.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use flux_lsp::config::ServerConfig;
use flux_lsp::install::{self, Source};
use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    AnyElement, App, AppContext as _, BoxShadow, ClipboardItem, Context, DismissEvent, Div, Entity,
    EventEmitter, FocusHandle, Focusable, FontWeight, KeyBinding, PathPromptOptions, Render,
    SharedString, Subscription, Task, WeakEntity, Window, actions, div, img, linear_color_stop,
    linear_gradient, point, prelude::*, px, svg,
};

use crate::appearance_settings::{AppearancePage, Page};
use crate::claude::{self, ClaudeStore, CliState};
use crate::editor_settings::EditorSettingsPage;
use crate::feedback;
use crate::i18n::{tr, trf};
use crate::icons::{self, IconName, file_icon, icon};
use crate::input::{InputEvent, TextInput};
use crate::lsp::LspStore;
use crate::motion::{self, TransitionExt};
use crate::notification_center::{self, Display};
use crate::plugin_manager::{PluginManager, PluginManagerEvent};
use crate::plugin_settings::PluginSettingsPage;
use crate::plugins::{PluginStatus, PluginStore};
use crate::region_guard::{self, Verdict};
use crate::region_status;
use crate::settings::{self, OpenProjectIn, UpdatePreference};
use crate::theme::{self, Theme, UiColors};
use crate::ui;
use crate::update_settings::UpdatesSettingsPage;
use crate::workspace::{Workspace, tilde};

actions!(settings, [Toggle, Dismiss, About]);

/// The window's size; a small Flux window gets a smaller one, with a margin around it.
const WIDTH: f32 = 880.;
const HEIGHT: f32 = 600.;
/// The margin between Settings and the sides of a small window (the height — `ui::dialog_height`).
const WINDOW_MARGIN: f32 = 16.;
const SIDEBAR_WIDTH: f32 = 180.;
/// The sidebar's rows: their height, the gap between them and the sidebar's padding — the selection
/// glides to the place they make.
const SIDEBAR_ROW_HEIGHT: f32 = 30.;
const SIDEBAR_ROW_GAP: f32 = 2.;
const SIDEBAR_PADDING: f32 = 8.;
/// A page coming in rises from this far below as it fades in.
const PAGE_RISE: f32 = 8.;
/// About: the logo tile and the column of row labels.
const ABOUT_LOGO_SIZE: f32 = 64.;
const ABOUT_LABEL_WIDTH: f32 = 120.;
/// Claude Code: the column of the CLI's facts (Executable, Version, Account).
const CLAUDE_LABEL_WIDTH: f32 = 150.;
/// Claude Code → Country Check: "checked 5 s ago" counts on this often while the page shows.
const CLOCK_TICK: Duration = Duration::from_secs(1);

/// The developer and the repository, for About.
const DEVELOPER: &str = "Egor Ageev";
const REPOSITORY: &str = "github.com/egor4geev/flux";

pub fn init(cx: &mut App) {
    crate::keymap::bind_keys(
        cx,
        [
            KeyBinding::new("cmd-,", Toggle, Some("Workspace")),
            KeyBinding::new("escape", Dismiss, Some("Settings")),
        ],
    );
    // Open Settings switch to About themselves (they have focus); otherwise the action reaches the
    // app, and the active window opens them on About.
    cx.on_action(about);
}

/// A section of Settings, listed on the left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Section {
    /// Appearance (stage 8.3), first: its pages are listed under it.
    Theme,
    /// The look of the window and its scale (part 10.4).
    Interface,
    FileIcons,
    Language,
    /// System Settings: how projects open (part 10.1).
    System,
    /// Updates of Flux (part 10.8).
    Updates,
    /// Soft wraps (part 10.5).
    Editor,
    LanguageServers,
    VersionControl,
    Notifications,
    /// The plugin manager.
    Plugins,
    /// A plugin's settings, by its id: listed under Plugins while the plugin is on.
    Plugin(SharedString),
    /// Claude Code: the chat and the CLI.
    Claude,
    About,
}

impl Section {
    /// The pages of Appearance, listed under it.
    const APPEARANCE: [Section; 4] = [
        Section::Theme,
        Section::Interface,
        Section::FileIcons,
        Section::Language,
    ];

    /// The sections above the plugins' pages; About goes after them.
    const BUILT_IN: [Section; 8] = [
        Section::System,
        Section::Updates,
        Section::Editor,
        Section::LanguageServers,
        Section::VersionControl,
        Section::Notifications,
        Section::Claude,
        Section::Plugins,
    ];

    fn label(&self) -> &'static str {
        match self {
            Section::Theme => tr("Theme"),
            Section::Interface => tr("Interface"),
            Section::FileIcons => tr("File Icons"),
            Section::Language => tr("Language"),
            Section::System => tr("System Settings"),
            Section::Updates => tr("Updates"),
            Section::Editor => tr("Editor"),
            Section::LanguageServers => tr("Language Servers"),
            Section::VersionControl => tr("Version Control"),
            Section::Notifications => tr("Notifications"),
            Section::Plugins | Section::Plugin(_) => tr("Plugins"),
            Section::Claude => tr("Claude Code"),
            Section::About => tr("About"),
        }
    }

    fn icon(&self) -> IconName {
        match self {
            Section::Theme | Section::Interface | Section::FileIcons | Section::Language => {
                IconName::Palette
            }
            Section::System => IconName::Settings,
            Section::Updates => IconName::Update,
            Section::Editor => IconName::Pencil,
            Section::LanguageServers => IconName::Command,
            Section::VersionControl => IconName::Branch,
            Section::Notifications => IconName::Bell,
            Section::Plugins | Section::Plugin(_) => IconName::Puzzle,
            Section::Claude => IconName::Claude,
            Section::About => IconName::Info,
        }
    }

    /// The page of Appearance this section shows.
    fn appearance_page(&self) -> Option<Page> {
        match self {
            Section::Theme => Some(Page::Theme),
            Section::Interface => Some(Page::Interface),
            Section::FileIcons => Some(Page::FileIcons),
            Section::Language => Some(Page::Language),
            _ => None,
        }
    }

    /// A stable key: the element ids of the sidebar.
    fn key(&self) -> SharedString {
        match self {
            Section::Theme => "theme".into(),
            Section::Interface => "interface".into(),
            Section::FileIcons => "file-icons".into(),
            Section::Language => "language".into(),
            Section::System => "system".into(),
            Section::Updates => "updates".into(),
            Section::Editor => "editor".into(),
            Section::LanguageServers => "language-servers".into(),
            Section::VersionControl => "version-control".into(),
            Section::Notifications => "notifications".into(),
            Section::Plugins => "plugins".into(),
            Section::Plugin(id) => format!("plugin-{id}").into(),
            Section::Claude => "claude".into(),
            Section::About => "about".into(),
        }
    }
}

/// Opens Settings, or closes them if open (the gear, ⌘,).
pub fn toggle(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    #[cfg(feature = "scenario")]
    let section = scenario_section().unwrap_or(Section::Theme);
    #[cfg(not(feature = "scenario"))]
    let section = Section::Theme;
    show(workspace, section, None, window, cx, false);
}

/// Opens Settings on a section; open ones are replaced.
pub fn open(
    workspace: &mut Workspace,
    section: Section,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    show(workspace, section, None, window, cx, true);
}

/// Opens Settings on Plugins with the plugin `id` selected: after it is installed, from a
/// notification about it.
pub fn open_plugin(
    workspace: &mut Workspace,
    id: SharedString,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    show(workspace, Section::Plugins, Some(id), window, cx, true);
}

/// Opens Settings (replacing open ones when `replace`, toggling them otherwise).
fn show(
    workspace: &mut Workspace,
    section: Section,
    plugin: Option<SharedString>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
    replace: bool,
) {
    if replace && is_open(workspace) {
        workspace.dismiss_modal(window, cx);
    }
    let handles = Handles {
        lsp: workspace.lsp.downgrade(),
        workspace: cx.weak_entity(),
        plugins: workspace.plugins.clone(),
        claude: workspace.claude.clone(),
    };
    workspace.toggle_dialog(window, cx, move |window, cx| {
        SettingsView::new(handles, section, plugin, window, cx)
    });
}

/// What Settings work with: the window's language servers and plugins, and the window itself
/// (the plugin manager's install flow).
struct Handles {
    lsp: WeakEntity<LspStore>,
    workspace: WeakEntity<Workspace>,
    plugins: Entity<PluginStore>,
    /// The window's Claude Code: the CLI found, its version and account.
    claude: Entity<ClaudeStore>,
}

/// With the `scenario` feature, `FLUX_SCENARIO_SETTINGS=plugins` (or `plugin:<id>`, `system`…)
/// opens Settings on that section: a scenario can't click the sidebar.
#[cfg(feature = "scenario")]
fn scenario_section() -> Option<Section> {
    let section = std::env::var("FLUX_SCENARIO_SETTINGS").ok()?;
    Some(match section.as_str() {
        "theme" => Section::Theme,
        "interface" => Section::Interface,
        "file-icons" => Section::FileIcons,
        "language" => Section::Language,
        "system" => Section::System,
        "updates" => Section::Updates,
        "editor" => Section::Editor,
        "language-servers" => Section::LanguageServers,
        "plugins" => Section::Plugins,
        "notifications" => Section::Notifications,
        "version-control" => Section::VersionControl,
        "claude" => Section::Claude,
        "about" => Section::About,
        other => Section::Plugin(other.strip_prefix("plugin:")?.to_string().into()),
    })
}

/// «About Flux» with Settings closed (or without focus in them): the active window opens them on
/// About — or the first window, when none is active (all of them minimized). The action arrives
/// while that window is being updated, so the window is updated after.
fn about(_: &About, cx: &mut App) {
    let window = cx
        .active_window()
        .into_iter()
        .chain(cx.windows())
        .find_map(|window| window.downcast::<Workspace>());
    let Some(window) = window else {
        return;
    };
    cx.defer(move |cx| {
        window
            .update(cx, |workspace, window, cx| {
                open(workspace, Section::About, window, cx)
            })
            .ok();
    });
}

pub struct SettingsView {
    focus_handle: FocusHandle,
    section: Section,
    /// The window's language servers: restarted after an install or update, stopped on delete.
    store: WeakEntity<LspStore>,
    /// The window's plugins: the manager and the plugins' pages follow them.
    plugins: Entity<PluginStore>,
    workspace: WeakEntity<Workspace>,
    /// The Plugins page, made when first shown.
    manager: Option<Entity<PluginManager>>,
    /// The plugin selected on the Plugins page when it is first shown.
    initial_plugin: Option<SharedString>,
    /// The pages of the plugins' settings, made when first shown, by plugin id.
    plugin_pages: HashMap<SharedString, Entity<PluginSettingsPage>>,
    /// The pages of Appearance, made when first shown.
    appearance: Option<Entity<AppearancePage>>,
    /// Settings → Editor (part 10.5), made when first shown.
    editor: Option<Entity<EditorSettingsPage>>,
    /// Settings → Updates (part 10.8), made when first shown.
    updates: Option<Entity<UpdatesSettingsPage>>,
    servers: Vec<ServerRow>,
    /// Operations in progress or failed, by server name.
    jobs: HashMap<String, Job>,
    /// The user's settings of servers the rows were made with (`lsp` in `settings.json`).
    user_servers: serde_json::Map<String, serde_json::Value>,
    /// The window's Claude Code.
    claude: Entity<ClaudeStore>,
    /// Claude Code's additional arguments, as the user types them.
    claude_args: Entity<TextInput>,
    /// The countries Claude is blocked in, as the user types them: saved once every word is a
    /// country code.
    countries: Entity<TextInput>,
    /// When About → Copy was last clicked: it says «Copied» for a moment.
    about_copied: ui::Copied,
    /// Where the segments of each segmented choice were drawn — a notification group's display, a
    /// default of Claude's new sessions — by the group's key or the choice's id: the chosen one's
    /// pill glides between them.
    display_choices: RefCell<HashMap<String, Rc<ui::Segments>>>,
    _tasks: Vec<Task<()>>,
    /// Redraws Claude Code's page every second: when the country was checked.
    _clock: Task<()>,
    _subscriptions: Vec<Subscription>,
}

struct ServerRow {
    config: ServerConfig,
    /// As the user set it up (`lsp.<server>` in `settings.json`): their own command is the one
    /// looked for; `None` — turned off.
    effective: Option<ServerConfig>,
    /// The language plugin that brings it, by its name in the interface language.
    plugin: SharedString,
    /// Where it comes from; `None` until the background check is done.
    source: Option<Source>,
    /// Whether Flux can install it here.
    installable: Result<(), String>,
}

#[derive(Debug, Clone)]
enum Job {
    Working {
        what: Operation,
        progress: Option<install::Progress>,
    },
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Operation {
    Install,
    Update,
    Delete,
}

impl EventEmitter<DismissEvent> for SettingsView {}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl SettingsView {
    fn new(
        handles: Handles,
        section: Section,
        plugin: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let servers = server_rows(&handles.plugins, cx);
        let claude_args = cx.new(|cx| {
            let mut input = TextInput::new("--add-dir ../shared", cx).code();
            input.set_text(&settings::claude(cx).extra_args.join(" "), cx);
            input
        });
        let countries = cx.new(|cx| {
            let mut input = TextInput::new("RU, BY", cx);
            input.set_text(&settings::claude(cx).blocked_countries.join(", "), cx);
            input
        });
        // A plugin turned off takes its page with it.
        let mut subscriptions = vec![
            cx.observe(&handles.plugins, |this, _, cx| {
                if let Section::Plugin(id) = &this.section
                    && !this.plugin_pages_listed(cx).contains(id)
                {
                    this.section = Section::Plugins;
                }
                cx.notify()
            }),
            cx.observe(&handles.claude, |_, _, cx| cx.notify()),
            // Language plugins turned on or off, installed, removed: other servers.
            cx.observe_global::<crate::contributions::Contributions>(|this, cx| {
                this.servers_changed(cx)
            }),
            // A server turned off or given its own command (`settings.json`, the switches).
            cx.observe_global::<settings::Settings>(|this, cx| {
                if settings::lsp(cx) != this.user_servers {
                    this.servers_changed(cx)
                }
            }),
            cx.subscribe(&claude_args, |this, input, _: &InputEvent, cx| {
                let args = input.read(cx).text();
                let args: Vec<String> = args.split_whitespace().map(str::to_string).collect();
                settings::update_claude(cx, |claude| claude.extra_args = args);
                this.claude.update(cx, |_, cx| cx.notify());
            }),
            // A typo doesn't change the list: it stays as it was until every word is a code (a
            // half-typed code would unblock a country for a moment).
            cx.subscribe(&countries, |_, input, _: &InputEvent, cx| {
                let text = input.read(cx).text();
                if flux_claude::region::invalid_codes(&text).is_empty() {
                    let codes = flux_claude::region::parse_codes(&text);
                    if codes != settings::claude(cx).blocked_countries {
                        settings::update_claude(cx, |claude| claude.blocked_countries = codes);
                    }
                }
                cx.notify();
            }),
        ];
        if let Some(guard) = region_guard::global(cx) {
            subscriptions.push(cx.observe(&guard, |_, _, cx| cx.notify()));
        }
        // An answer that only refreshes the time doesn't notify: the clock redraws the page.
        let clock = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(CLOCK_TICK).await;
                let alive = this.update(cx, |this, cx| {
                    if this.section == Section::Claude
                        && region_guard::verdict(cx).checked_at().is_some()
                    {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        let mut view = Self {
            focus_handle: cx.focus_handle(),
            section: Section::Theme,
            store: handles.lsp,
            plugins: handles.plugins,
            workspace: handles.workspace,
            manager: None,
            initial_plugin: plugin,
            plugin_pages: HashMap::new(),
            appearance: None,
            editor: None,
            updates: None,
            servers,
            jobs: HashMap::new(),
            user_servers: settings::lsp(cx),
            claude: handles.claude,
            claude_args,
            countries,
            about_copied: ui::Copied::default(),
            display_choices: RefCell::default(),
            _tasks: Vec::new(),
            _clock: clock,
            _subscriptions: subscriptions,
        };
        view.refresh(cx);
        view.select(section, window, cx);
        view
    }

    /// Where each server comes from: file checks (and `rustup which`) in the background.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let configs: Vec<ServerConfig> = self
            .servers
            .iter()
            .map(|row| row.effective.clone().unwrap_or_else(|| row.config.clone()))
            .collect();
        let task = cx.spawn(async move |this, cx| {
            let found = cx
                .background_spawn(async move {
                    configs
                        .iter()
                        .map(|config| {
                            let found = (install::source(config), install::check(config));
                            (config.name.clone(), found)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            this.update(cx, |this, cx| {
                // By name: the rows may have changed meanwhile (a language plugin installed).
                for (name, (source, installable)) in found {
                    if let Some(row) = this.servers.iter_mut().find(|row| row.config.name == name) {
                        row.source = Some(source);
                        row.installable = installable;
                    }
                }
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    /// The language plugins bring other servers: the rows follow (a row that stays keeps what is
    /// known about it), then where they come from is checked again.
    fn servers_changed(&mut self, cx: &mut Context<Self>) {
        self.user_servers = settings::lsp(cx);
        let mut rows = server_rows(&self.plugins, cx);
        for row in &mut rows {
            if let Some(known) = self
                .servers
                .iter()
                .find(|known| known.config == row.config && known.effective == row.effective)
            {
                row.source = known.source.clone();
                row.installable = known.installable.clone();
            }
        }
        self.servers = rows;
        self.refresh(cx);
        cx.notify();
    }

    fn set_auto_install(&mut self, on: bool, cx: &mut Context<Self>) {
        settings::set_auto_install_servers(on, cx);
        if on {
            self.store
                .update(cx, |store, cx| store.retry_not_installed(cx))
                .ok();
        }
        cx.notify();
    }

    /// Install, update, or delete in the background; then the window's servers follow.
    fn run(&mut self, index: usize, what: Operation, cx: &mut Context<Self>) {
        let config = self.servers[index].config.clone();
        let name = config.name.clone();
        if matches!(self.jobs.get(&name), Some(Job::Working { .. })) {
            return;
        }
        self.jobs.insert(
            name.clone(),
            Job::Working {
                what,
                progress: None,
            },
        );
        if what == Operation::Delete {
            // Not running from a deleted directory.
            self.store
                .update(cx, |store, cx| store.stop_named(&name, cx))
                .ok();
        }
        let (sender, mut receiver) = mpsc::unbounded();
        let work = cx.background_spawn(async move {
            let progress = move |progress: install::Progress| {
                sender.unbounded_send(progress).ok();
            };
            let cancel = AtomicBool::new(false);
            match what {
                Operation::Install => install::install(&config, &progress, &cancel),
                Operation::Update => install::update(&config, &progress, &cancel),
                Operation::Delete => install::uninstall(&config),
            }
        });
        let task = cx.spawn(async move |this, cx| {
            while let Some(mut progress) = receiver.next().await {
                while let Ok(newer) = receiver.try_recv() {
                    progress = newer;
                }
                let shown = this.update(cx, |this, cx| {
                    if let Some(Job::Working {
                        progress: shown, ..
                    }) = this.jobs.get_mut(&name)
                    {
                        *shown = Some(progress);
                        cx.notify();
                    }
                });
                if shown.is_err() {
                    return;
                }
            }
            let result = work.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.jobs.remove(&name);
                        if what != Operation::Delete {
                            this.store
                                .update(cx, |store, cx| store.restart_named(&name, cx))
                                .ok();
                        }
                    }
                    Err(reason) => {
                        this.jobs.insert(name.clone(), Job::Failed(reason));
                    }
                }
                this.refresh(cx);
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
        cx.notify();
    }

    fn render_server(&self, index: usize, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let row = &self.servers[index];
        let config = &row.config;
        let files = config
            .extensions
            .iter()
            .map(|extension| format!(".{extension}"))
            .chain(config.file_names.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ");
        let languages = if files.is_empty() {
            row.plugin.to_string()
        } else {
            format!("{} · {files}", row.plugin)
        };
        let file = file_icon(
            &format!("x.{}", config.extensions.first().map_or("", String::as_str)),
            &ui,
        );
        let job = self.jobs.get(&config.name);
        let enabled = row.effective.is_some();
        let own_command = crate::lsp_settings::own_command(&config.name, cx);
        // The user's settings change something (an empty key "Configure…" added doesn't).
        let custom = row
            .effective
            .as_ref()
            .is_some_and(|effective| effective != config);
        let (status, status_color): (String, _) = match (job, &row.source) {
            (None, _) if !enabled => (tr("Turned off: Flux doesn't start it").to_string(), ui.dim),
            (None, Some(Source::Missing)) if own_command.is_some() => (
                trf("Not found: {0}", &[&own_command.unwrap_or_default()]),
                ui.error,
            ),
            (Some(Job::Working { what, progress }), _) => {
                let mut text = match what {
                    Operation::Install => tr("Installing…").to_string(),
                    Operation::Update => tr("Updating…").to_string(),
                    Operation::Delete => tr("Deleting…").to_string(),
                };
                if let Some(fraction) = progress.as_ref().and_then(|progress| progress.fraction) {
                    text = format!("{text} {}%", (fraction.clamp(0., 1.) * 100.).round() as u32);
                }
                (text, ui.accent_text)
            }
            (Some(Job::Failed(reason)), _) => {
                let first = reason.lines().next().unwrap_or_default();
                (trf("Failed: {0}", &[&first]), ui.error)
            }
            (None, None) => (tr("Checking…").to_string(), ui.dim),
            (None, Some(Source::System(path))) => {
                (trf("On this Mac · {0}", &[&tilde(path)]), ui.text_muted)
            }
            (None, Some(Source::Flux { version })) => (
                match version {
                    Some(version) => trf("Installed by Flux · {0}", &[version]),
                    None => tr("Installed by Flux").to_string(),
                },
                ui.success,
            ),
            (None, Some(Source::Missing)) => match &row.installable {
                Ok(()) => (tr("Not installed").to_string(), ui.dim),
                Err(reason) => (
                    trf("Not installed: {0}", &[&crate::lsp::install_reason(reason)]),
                    ui.dim,
                ),
            },
        };
        let idle = !matches!(job, Some(Job::Working { .. }));
        let buttons: Vec<gpui::AnyElement> = match &row.source {
            _ if !idle => Vec::new(),
            Some(Source::Flux { .. }) => vec![
                ui::text_button(("update", index), tr("Update"), false, ui)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.run(index, Operation::Update, cx)),
                    )
                    .into_any_element(),
                ui::text_button(("delete", index), tr("Delete"), true, ui)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.run(index, Operation::Delete, cx)),
                    )
                    .into_any_element(),
            ],
            Some(Source::Missing) if row.installable.is_ok() && enabled => vec![
                ui::text_button(("install", index), tr("Install"), false, ui)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.run(index, Operation::Install, cx)),
                    )
                    .into_any_element(),
            ],
            _ => Vec::new(),
        };
        // Its key in `settings.json` (part 10.5), and on or off.
        let name = config.name.clone();
        let configure = ui::text_button(("configure", index), tr("Configure…"), false, ui)
            .on_click(
                cx.listener(move |this, _, window, cx| this.configure(name.clone(), window, cx)),
            );
        let name = config.name.clone();
        let switch = ui::switch(("server-on", index), enabled, ui)
            .tooltip(ui::tooltip(
                if enabled {
                    tr("Turn off")
                } else {
                    tr("Turn on")
                },
                None,
            ))
            .on_click(cx.listener(move |_, _, _, cx| {
                crate::lsp_settings::set_enabled(&name, !enabled, cx)
            }));
        let mark = if !enabled {
            Some(ui::badge(tr("Off"), ui.dim))
        } else if custom {
            Some(ui::badge(tr("Custom"), ui.accent_text))
        } else {
            None
        };
        div()
            // The id lets the hover fade.
            .id(("server-row", index))
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            .rounded(px(ui::RADIUS_MD))
            .hover(move |style| style.bg(ui.hover))
            .child(file.render().size(px(16.)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap_2()
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .when(!enabled, |name| name.text_color(ui.text_muted))
                                    .child(config.name.clone()),
                            )
                            .children(mark)
                            .child(
                                div()
                                    .text_size(px(theme::TEXT_XS))
                                    .text_color(ui.dim)
                                    .truncate()
                                    .child(languages),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(status_color)
                            .truncate()
                            .child(status),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .children(buttons)
                    .child(configure)
                    .child(div().w(px(6.)))
                    .child(switch),
            )
    }

    /// "Configure…" of a server: Settings close, and `settings.json` opens in a tab at its key.
    fn configure(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        cx.emit(DismissEvent);
        window.defer(cx, move |window, cx| {
            workspace.update(cx, |workspace, cx| {
                crate::lsp_settings::configure(workspace, &name, window, cx)
            });
        });
    }

    /// "Open settings.json" (Language Servers): Settings close, the file opens in a tab.
    fn open_settings_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        cx.emit(DismissEvent);
        window.defer(cx, move |window, cx| {
            workspace.update(cx, |workspace, cx| {
                crate::settings_file::open(workspace, None, window, cx)
            });
        });
    }
}

impl SettingsView {
    /// Shows a section; the Plugins page and the plugins' pages are made when first shown. The
    /// Plugins page puts the focus into its search field, as JetBrains IDEs do.
    fn select(&mut self, section: Section, window: &mut Window, cx: &mut Context<Self>) {
        match &section {
            Section::Plugins => {
                let manager = self.manager(window, cx);
                if let Some(id) = self.initial_plugin.take() {
                    manager.update(cx, |manager, cx| manager.reveal(id, cx));
                }
                // The window's modal layer focuses Settings once they are made: the search takes
                // the focus after that.
                let manager = manager.downgrade();
                window.defer(cx, move |window, cx| {
                    manager
                        .update(cx, |manager, cx| manager.focus_search(window, cx))
                        .ok();
                });
            }
            Section::Plugin(id) => {
                if !self.plugin_pages.contains_key(id) {
                    let plugins = self.plugins.clone();
                    let id = id.clone();
                    let page =
                        cx.new(|cx| PluginSettingsPage::new(id.clone(), plugins, window, cx));
                    self.plugin_pages.insert(id, page);
                }
                window.focus(&self.focus_handle);
            }
            Section::Theme | Section::Interface | Section::FileIcons | Section::Language => {
                let page = section.appearance_page().unwrap_or(Page::Theme);
                match &self.appearance {
                    Some(appearance) => {
                        appearance.update(cx, |appearance, cx| appearance.show(page, cx))
                    }
                    None => {
                        let appearance = cx.new(|cx| AppearancePage::new(page, window, cx));
                        self.appearance = Some(appearance);
                    }
                }
                if !self.focus_handle.is_focused(window) {
                    window.focus(&self.focus_handle);
                }
            }
            Section::Editor => {
                if self.editor.is_none() {
                    self.editor = Some(cx.new(|cx| EditorSettingsPage::new(window, cx)));
                }
                if !self.focus_handle.is_focused(window) {
                    window.focus(&self.focus_handle);
                }
            }
            Section::Updates => {
                if self.updates.is_none() {
                    self.updates = Some(cx.new(|cx| UpdatesSettingsPage::new(window, cx)));
                }
                if !self.focus_handle.is_focused(window) {
                    window.focus(&self.focus_handle);
                }
            }
            _ => {
                // The focus leaves a field of the page that goes away (Esc must still close).
                if !self.focus_handle.is_focused(window) {
                    window.focus(&self.focus_handle);
                }
            }
        }
        if self.section != section {
            self.section = section;
            cx.notify();
        }
    }

    /// The Plugins page: made once, then kept with its search and selection.
    fn manager(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<PluginManager> {
        if let Some(manager) = &self.manager {
            return manager.clone();
        }
        let plugins = self.plugins.clone();
        let workspace = self.workspace.clone();
        let manager = cx.new(|cx| PluginManager::new(plugins, workspace, window, cx));
        cx.subscribe_in(&manager, window, |this, _, event, window, cx| match event {
            PluginManagerEvent::OpenSettings(id) => {
                this.select(Section::Plugin(id.clone()), window, cx)
            }
        })
        .detach();
        self.manager = Some(manager.clone());
        manager
    }

    /// The plugins with a page under Plugins: on, and with settings in the manifest.
    fn plugin_pages_listed(&self, cx: &App) -> Vec<SharedString> {
        self.plugins
            .read(cx)
            .plugins()
            .iter()
            .filter(|plugin| !plugin.entry.manifest.settings.is_empty())
            .filter(|plugin| {
                !matches!(
                    plugin.status,
                    PluginStatus::Disabled | PluginStatus::Unavailable(_)
                )
            })
            .map(|plugin| SharedString::from(plugin.id().to_string()))
            .collect()
    }

    /// The list of sections on the left; the open one is highlighted. The plugins' pages are
    /// indented under Plugins, as the settings tree of JetBrains IDEs, each with the plugin's icon.
    fn render_sidebar(&self, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let store = self.plugins.read(cx);
        let plugin_pages: Vec<(Section, SharedString, Option<SharedString>)> = self
            .plugin_pages_listed(cx)
            .into_iter()
            .filter_map(|id| {
                let plugin = store.plugin(&id)?;
                let manifest = &plugin.entry.manifest;
                let name = plugin.tr(&manifest.name).to_string();
                let icon = (manifest.icon.as_deref()).map(|file| icons::plugin_asset(&id, file));
                Some((Section::Plugin(id), SharedString::from(name), icon))
            })
            .collect();
        // A plugin's page shows its icon from the manifest (a puzzle piece without one) in the
        // icons' column, so its name lines up with the rest; the other nested pages have none.
        let row =
            |section: Section,
             label: SharedString,
             nested: bool,
             plugin_icon: Option<SharedString>,
             cx: &mut Context<Self>| {
                let selected = section == self.section;
                let icon_color = if selected { ui.accent_text } else { ui.dim };
                let with_icon = !nested || matches!(section, Section::Plugin(_));
                let icon_name = section.icon();
                div()
                    .id(SharedString::from(format!(
                        "settings-section-{}",
                        section.key()
                    )))
                    .h(px(SIDEBAR_ROW_HEIGHT))
                    .px_2()
                    .when(nested && !with_icon, |row| row.pl(px(30.)))
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(px(ui::RADIUS_SM))
                    .cursor_pointer()
                    .text_color(if selected {
                        ui.foreground
                    } else {
                        ui.text_muted
                    })
                    // The selection's backdrop is the sidebar's own, gliding from row to row.
                    .when(!selected, |row| {
                        row.hover(move |style| style.bg(ui.hover).text_color(ui.foreground))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select(section.clone(), window, cx)
                    }))
                    .when(with_icon, |row| match plugin_icon {
                        Some(path) => row.child(
                            svg()
                                .path(path)
                                .flex_none()
                                .size(px(14.))
                                .text_color(icon_color),
                        ),
                        None => row.child(icon(icon_name, icon_color).flex_none().size(px(14.))),
                    })
                    .child(div().min_w_0().truncate().child(label))
            };
        // Appearance: a heading (it opens its first page) with its pages under it.
        let appearance_open = Section::APPEARANCE.contains(&self.section);
        let mut rows: Vec<AnyElement> = vec![
            div()
                .id("settings-section-appearance")
                .h(px(SIDEBAR_ROW_HEIGHT))
                .px_2()
                .flex()
                .items_center()
                .gap_2()
                .rounded(px(ui::RADIUS_SM))
                .cursor_pointer()
                .text_color(if appearance_open {
                    ui.foreground
                } else {
                    ui.text_muted
                })
                .hover(move |style| style.bg(ui.hover).text_color(ui.foreground))
                .on_click(
                    cx.listener(|this, _, window, cx| this.select(Section::Theme, window, cx)),
                )
                .child(
                    icon(
                        IconName::Palette,
                        if appearance_open {
                            ui.accent_text
                        } else {
                            ui.dim
                        },
                    )
                    .size(px(14.)),
                )
                .child(div().min_w_0().truncate().child(tr("Appearance")))
                .into_any_element(),
        ];
        // The selected row's place among them: the Appearance heading, its pages, the built-in
        // sections, the plugins' pages, About.
        let selected = Section::APPEARANCE
            .iter()
            .position(|section| *section == self.section)
            .map(|index| 1 + index)
            .or_else(|| {
                let index = Section::BUILT_IN
                    .iter()
                    .position(|section| *section == self.section)?;
                Some(1 + Section::APPEARANCE.len() + index)
            })
            .or_else(|| {
                let index = plugin_pages
                    .iter()
                    .position(|(section, ..)| *section == self.section)?;
                Some(1 + Section::APPEARANCE.len() + Section::BUILT_IN.len() + index)
            })
            .or_else(|| {
                (self.section == Section::About).then(|| {
                    1 + Section::APPEARANCE.len() + Section::BUILT_IN.len() + plugin_pages.len()
                })
            });
        rows.extend(Section::APPEARANCE.into_iter().map(|section| {
            let label = SharedString::from(section.label());
            row(section, label, true, None, cx).into_any_element()
        }));
        rows.extend(Section::BUILT_IN.into_iter().map(|section| {
            let label = SharedString::from(section.label());
            row(section, label, false, None, cx).into_any_element()
        }));
        rows.extend(
            plugin_pages.into_iter().map(|(section, label, icon)| {
                row(section, label, true, icon, cx).into_any_element()
            }),
        );
        rows.push(row(Section::About, tr("About").into(), false, None, cx).into_any_element());
        // The selection's backdrop, under the rows: it glides to the row chosen (`motion::SMOOTH`).
        let highlight = selected.map(|index| {
            let top = SIDEBAR_PADDING + index as f32 * (SIDEBAR_ROW_HEIGHT + SIDEBAR_ROW_GAP);
            div()
                .absolute()
                .left(px(SIDEBAR_PADDING))
                .right(px(SIDEBAR_PADDING))
                .h(px(SIDEBAR_ROW_HEIGHT))
                .rounded(px(ui::RADIUS_SM))
                .bg(ui.list_selected)
                .with_transition(
                    "settings-selection",
                    top,
                    motion::SMOOTH,
                    |highlight, top| highlight.top(px(top)),
                )
        });
        div()
            .flex_none()
            .relative()
            .w(px(SIDEBAR_WIDTH))
            .p(px(SIDEBAR_PADDING))
            .flex()
            .flex_col()
            .gap(px(SIDEBAR_ROW_GAP))
            .border_r_1()
            .border_color(ui.divider)
            .children(highlight)
            .children(rows)
    }

    fn render_language_servers(&self, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let auto = settings::auto_install_servers(cx);
        let servers_dir = dirs_label();
        let rows: Vec<AnyElement> = (0..self.servers.len())
            .map(|index| self.render_server(index, cx).into_any_element())
            .collect();
        // No language plugin brings a server: they are in the Marketplace.
        let rows = if rows.is_empty() {
            vec![
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .p_3()
                    .rounded(px(ui::RADIUS_MD))
                    .border_1()
                    .border_color(ui.island_border)
                    .child(icon(IconName::Puzzle, ui.dim).size(px(16.)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(tr("No language servers yet"))
                            .child(
                                div()
                                    .text_size(px(theme::TEXT_SM))
                                    .text_color(ui.dim)
                                    .child(tr(
                                        "Language plugins bring them: install one for your language in Settings → Plugins.",
                                    )),
                            ),
                    )
                    .child(
                        ui::text_button("open-plugins", tr("Open Plugins"), false, ui).on_click(
                            cx.listener(|this, _, window, cx| {
                                this.select(Section::Plugins, window, cx)
                            }),
                        ),
                    )
                    .into_any_element(),
            ]
        } else {
            rows
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(section_header(
                tr("Language Servers"),
                tr(
                    "Flux starts the server for the language of an open file: errors, completion, go to definition. Servers installed on this Mac are used as they are; those Flux installed can be updated or deleted here.",
                ),
                ui,
            ))
            .child(
                div()
                    .id("auto-install")
                    .flex()
                    .items_center()
                    .gap_3()
                    .p_3()
                    .rounded(px(ui::RADIUS_MD))
                    .border_1()
                    .border_color(ui.island_border)
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.set_auto_install(!auto, cx)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(tr("Install missing servers automatically"))
                            .child(
                                div()
                                    .text_size(px(theme::TEXT_SM))
                                    .text_color(ui.dim)
                                    .child(tr(
                                        "When a file needs a server that isn't on this Mac, Flux downloads it to:",
                                    ))
                                    .child(
                                        div()
                                            .font_family(theme::code_font())
                                            .text_size(px(theme::TEXT_XS))
                                            .child(servers_dir),
                                    ),
                            ),
                    )
                    .child(ui::switch("auto-install-switch", auto, ui)),
            )
            // Part 10.5: what each server is given lives in `settings.json`.
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .p_3()
                    .rounded(px(ui::RADIUS_MD))
                    .border_1()
                    .border_color(ui.island_border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(tr("Server settings"))
                            .child(
                                div()
                                    .text_size(px(theme::TEXT_SM))
                                    .text_color(ui.dim)
                                    .child(tr(
                                        "Each server's settings, command and environment are in settings.json, under “lsp”: Configure… opens a server's key.",
                                    )),
                            ),
                    )
                    .child(
                        ui::text_button("open-settings-file", tr("Open settings.json"), false, ui)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_settings_file(window, cx)
                            })),
                    ),
            )
            .child(ui::divider(ui))
            .child(div().flex().flex_col().gap_0p5().children(rows))
    }

    /// Version Control: Update Project's method, as JetBrains IDEs' Settings → Git → Update.
    fn render_version_control(&self, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let current = settings::update_method(cx);
        let options = [
            (
                UpdatePreference::Merge,
                tr("Merge"),
                tr("Incoming commits are merged into the current branch"),
            ),
            (
                UpdatePreference::Rebase,
                tr("Rebase"),
                tr("Your local commits are replayed on top of the incoming ones"),
            ),
            (
                UpdatePreference::Ask,
                tr("Ask every time"),
                tr("Update Project asks before it starts"),
            ),
        ];
        let rows = options
            .into_iter()
            .enumerate()
            .map(|(index, (method, title, detail))| {
                let selected = current == method;
                div()
                    .id(("update-method", index))
                    .flex()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded(px(ui::RADIUS_MD))
                    .cursor_pointer()
                    .hover(move |style| style.bg(ui.hover))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        settings::set_update_method(method, cx);
                        cx.notify();
                    }))
                    .child(div().pt(px(2.)).child(ui::radio(
                        ("update-method-radio", index),
                        selected,
                        ui,
                    )))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(title)
                            .child(
                                div()
                                    .text_size(px(theme::TEXT_SM))
                                    .text_color(ui.dim)
                                    .child(detail),
                            ),
                    )
            });
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(section_header(
                tr("Version Control"),
                trf(
                    "Git: how Update Project ({0}) brings the commits of the upstream into the current branch. Local changes are stashed for the update and come back after it.",
                    &[&crate::keymap::shortcut("git::UpdateProject", "cmd-t")],
                ),
                ui,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_1()
                    .rounded(px(ui::RADIUS_MD))
                    .border_1()
                    .border_color(ui.island_border)
                    .child(
                        div()
                            .px_3()
                            .pt_2()
                            .pb_1()
                            .child(ui::section_label(tr("Update Project"), ui)),
                    )
                    .children(rows),
            )
    }

    /// System Settings: reopening the projects on startup, and where another project opens.
    fn render_system(&self, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let projects = settings::projects(cx);
        let reopen = projects.reopen;
        let options = [
            (
                OpenProjectIn::NewWindow,
                tr("New window"),
                tr("The project gets a window of its own; the open one stays as it is"),
            ),
            (
                OpenProjectIn::ThisWindow,
                tr("This window"),
                tr(
                    "The open project closes, keeping its tabs for next time, and the new one takes its place",
                ),
            ),
            (
                OpenProjectIn::Ask,
                tr("Ask"),
                tr("Flux asks every time; “Don’t ask again” remembers the answer here"),
            ),
        ];
        let rows = options
            .into_iter()
            .enumerate()
            .map(|(index, (choice, title, detail))| {
                let selected = projects.open_in == choice;
                div()
                    .id(("open-project-in", index))
                    .flex()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded(px(ui::RADIUS_MD))
                    .cursor_pointer()
                    .hover(move |style| style.bg(ui.hover))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        settings::update_projects(cx, |projects| projects.open_in = choice);
                        cx.notify();
                    }))
                    .child(div().pt(px(2.)).child(ui::radio(
                        ("open-project-in-radio", index),
                        selected,
                        ui,
                    )))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(title)
                            .child(
                                div()
                                    .text_size(px(theme::TEXT_SM))
                                    .text_color(ui.dim)
                                    .child(detail),
                            ),
                    )
            });
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(section_header(
                tr("System Settings"),
                tr("How Flux opens projects: when it starts, and when the window already has one."),
                ui,
            ))
            .child(
                div()
                    .id("reopen-projects")
                    .flex()
                    .items_center()
                    .gap_3()
                    .p_3()
                    .rounded(px(ui::RADIUS_MD))
                    .border_1()
                    .border_color(ui.island_border)
                    .cursor_pointer()
                    .on_click(cx.listener(move |_, _, _, cx| {
                        settings::update_projects(cx, |projects| projects.reopen = !reopen);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(tr("Reopen projects on startup"))
                            .child(
                                div()
                                    .text_size(px(theme::TEXT_SM))
                                    .text_color(ui.dim)
                                    .child(tr(
                                        "Flux started from the Dock or the Finder opens the projects that were open when it quit.",
                                    )),
                            ),
                    )
                    .child(ui::switch("reopen-projects-switch", reopen, ui)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_1()
                    .rounded(px(ui::RADIUS_MD))
                    .border_1()
                    .border_color(ui.island_border)
                    .child(
                        div()
                            .px_3()
                            .pt_2()
                            .pb_1()
                            .child(ui::section_label(tr("Open project in"), ui)),
                    )
                    .children(rows),
            )
    }

    /// Notifications: Do Not Disturb, then a row per group with its display.
    fn render_notifications(&self, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let quiet = settings::do_not_disturb(cx);
        let rows: Vec<_> = notification_center::groups(cx)
            .into_iter()
            .enumerate()
            .map(|(index, info)| {
                let key = info.group.key();
                let current = notification_center::display_of(&info.group, cx);
                let segments = self
                    .display_choices
                    .borrow_mut()
                    .entry(key.to_string())
                    .or_default()
                    .clone();
                let pill = segments.pill(
                    Display::ALL.iter().position(|display| *display == current),
                    "display-pill",
                    ui,
                );
                let own_backdrop = pill.is_none();
                let choices = Display::ALL
                    .into_iter()
                    .enumerate()
                    .map(|(place, display)| {
                        let key = key.clone();
                        let selected = display == current;
                        div()
                            .id(SharedString::from(format!(
                                "display-{index}-{}",
                                display.key()
                            )))
                            .px_2()
                            .h(px(24.))
                            .flex()
                            .items_center()
                            .rounded(px(ui::RADIUS_SM))
                            .text_size(px(theme::TEXT_SM))
                            .cursor_pointer()
                            .when(selected, |choice| choice.text_color(ui.foreground))
                            .when(selected && own_backdrop, |choice| {
                                choice.bg(ui.list_selected)
                            })
                            .when(!selected, |choice| {
                                choice.text_color(ui.text_muted).hover(move |style| {
                                    style.bg(ui.hover).text_color(ui.foreground)
                                })
                            })
                            .on_click(cx.listener(move |_, _, _, cx| {
                                settings::set_notification_display(&key, display, cx);
                                cx.notify();
                            }))
                            .child(segments.measure(place))
                            .child(display.label())
                    });
                div()
                    // The id lets the hover fade.
                    .id(("notification-group", index))
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_1p5()
                    .rounded(px(ui::RADIUS_MD))
                    .hover(move |style| style.bg(ui.hover))
                    .child(div().flex_1().min_w_0().truncate().child(info.title))
                    // A segmented choice: one of four, the chosen one highlighted.
                    .child(
                        div()
                            .flex_none()
                            .relative()
                            .flex()
                            .gap_0p5()
                            .p_0p5()
                            .rounded(px(ui::RADIUS_MD))
                            .bg(ui.input_background)
                            .border_1()
                            .border_color(ui.input_border)
                            .child(segments.measure_origin())
                            .children(pill)
                            .children(choices),
                    )
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(section_header(
                tr("Notifications"),
                tr(
                    "Results of operations, errors and background tasks go to the Notifications window; a card in the corner shows them as they happen. Choose how each group shows.",
                ),
                ui,
            ))
            .child(
                div()
                    .id("do-not-disturb")
                    .flex()
                    .items_center()
                    .gap_3()
                    .p_3()
                    .rounded(px(ui::RADIUS_MD))
                    .border_1()
                    .border_color(ui.island_border)
                    .cursor_pointer()
                    .on_click(cx.listener(move |_, _, _, cx| {
                        settings::set_do_not_disturb(!quiet, cx);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(tr("Do not disturb"))
                            .child(
                                div()
                                    .text_size(px(theme::TEXT_SM))
                                    .text_color(ui.dim)
                                    .child(tr(
                                        "No cards in the corner; notifications still go to the Notifications window.",
                                    )),
                            ),
                    )
                    .child(ui::switch("do-not-disturb-switch", quiet, ui)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .p_1()
                    .rounded(px(ui::RADIUS_MD))
                    .border_1()
                    .border_color(ui.island_border)
                    .when(quiet, |groups| groups.opacity(0.6))
                    .child(
                        div()
                            .px_3()
                            .pt_2()
                            .pb_1()
                            .child(ui::section_label(tr("Groups"), ui)),
                    )
                    .children(rows),
            )
    }

    /// Claude Code: on or off, the CLI, the defaults of new sessions, the chat's switches, more
    /// arguments for the CLI.
    fn render_claude(&self, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let current = settings::claude(cx);
        let store = self.claude.read(cx);
        let state = store.cli().clone();
        let switch_row = |id: &'static str,
                          title: &'static str,
                          detail: SharedString,
                          on: bool,
                          change: fn(&mut settings::ClaudeSettings, bool),
                          cx: &mut Context<Self>| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap_3()
                .px_3()
                .py_2()
                .rounded(px(ui::RADIUS_MD))
                .cursor_pointer()
                .hover(move |style| style.bg(ui.hover))
                .on_click(cx.listener(move |this, _, _, cx| {
                    settings::update_claude(cx, |claude| change(claude, !on));
                    this.claude.update(cx, |_, cx| cx.notify());
                    cx.notify();
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(title)
                        .child(
                            div()
                                .text_size(px(theme::TEXT_SM))
                                .text_color(ui.dim)
                                .child(detail),
                        ),
                )
                .child(ui::switch(
                    SharedString::from(format!("{id}-switch")),
                    on,
                    ui,
                ))
        };
        let card = |label: &'static str| {
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .p_1()
                .rounded(px(ui::RADIUS_MD))
                .border_1()
                .border_color(ui.island_border)
                .child(
                    div()
                        .px_3()
                        .pt_2()
                        .pb_1()
                        .child(ui::section_label(label, ui)),
                )
        };
        // A row with a label on the left and a value (text, buttons) on the right.
        let fact = |label: &'static str, value: AnyElement| {
            div()
                .flex()
                .items_center()
                .gap_3()
                .px_3()
                .py_1p5()
                .child(
                    div()
                        .flex_none()
                        .w(px(CLAUDE_LABEL_WIDTH))
                        .text_color(ui.text_muted)
                        .child(label),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(value),
                )
        };
        // The CLI: where it is, its version and account.
        // A long path is shortened in the middle: the file name stays.
        let path_label = |path: &std::path::Path| crate::claude::shorten(&tilde(path), 56);
        let (path_text, path_color) = match (&current.path, state.cli()) {
            (Some(path), _) => (path_label(path), ui.foreground),
            (None, Some(cli)) => (path_label(&cli.path), ui.foreground),
            (None, None) if state == CliState::Checking => (tr("Looking…").to_string(), ui.dim),
            (None, None) => (tr("Not found").to_string(), ui.warning),
        };
        let custom = current.path.is_some();
        let path_value = div()
            .flex()
            .items_center()
            .gap_2()
            .min_w_0()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(theme::code_font())
                    .text_size(px(theme::TEXT_SM))
                    .text_color(path_color)
                    .child(path_text),
            )
            .when(!custom && state.cli().is_some(), |row| {
                row.child(ui::badge(tr("found"), ui.dim))
            })
            .child(
                ui::text_button("claude-path-choose", tr("Choose…"), false, ui)
                    .on_click(cx.listener(|this, _, window, cx| this.choose_claude(window, cx))),
            )
            .when(custom, |row| {
                row.child(
                    ui::text_button("claude-path-reset", tr("Reset"), false, ui).on_click(
                        cx.listener(|this, _, _, cx| {
                            settings::update_claude(cx, |claude| claude.path = None);
                            this.claude.update(cx, |store, cx| store.check(cx));
                        }),
                    ),
                )
            })
            .into_any_element();
        let (version, account): (SharedString, SharedString) = match &state {
            CliState::Checking => (tr("Checking…").into(), "".into()),
            CliState::Missing => ("—".into(), "—".into()),
            CliState::SignedOut { version, .. } => (
                version.clone().unwrap_or_else(|| "—".to_string()).into(),
                tr("Signed out").into(),
            ),
            CliState::Ready {
                version, account, ..
            } => (
                version.clone().into(),
                account
                    .clone()
                    .unwrap_or_else(|| tr("Signed in").to_string())
                    .into(),
            ),
            CliState::Failed(error) => (error.clone().into(), "—".into()),
            CliState::NoGitBash => (tr("Claude Code needs Git for Windows").into(), "—".into()),
        };
        let signed_out = matches!(state, CliState::SignedOut { .. });
        let account_value = div()
            .flex()
            .items_center()
            .gap_2()
            .child(div().min_w_0().truncate().child(account))
            .when(signed_out, |row| {
                row.child(
                    ui::text_button("claude-sign-in", tr("Sign In"), false, ui).on_click(
                        |_, window, cx| window.dispatch_action(Box::new(claude::SignIn), cx),
                    ),
                )
            })
            .into_any_element();
        let cli_card = card(tr("Command Line Tool"))
            .child(fact(tr("Executable"), path_value))
            .child(fact(
                tr("Version"),
                div().min_w_0().truncate().child(version).into_any_element(),
            ))
            .child(fact(tr("Account"), account_value))
            .child(div().px_3().pt_1().pb_2().flex().child(
                ui::text_button("claude-check-again", tr("Check Again"), false, ui).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.claude.update(cx, |store, cx| store.check(cx))
                    }),
                ),
            ));
        // The defaults of new sessions: segmented choices.
        let choice_row = |id: &'static str,
                          label: &'static str,
                          options: Vec<(Option<&'static str>, &'static str)>,
                          selected: Option<String>,
                          set: fn(&mut settings::ClaudeSettings, Option<String>),
                          cx: &mut Context<Self>| {
            // The chosen one's pill glides to the next one chosen.
            let segments = self
                .display_choices
                .borrow_mut()
                .entry(id.to_string())
                .or_default()
                .clone();
            let pill = segments.pill(
                options
                    .iter()
                    .position(|(value, _)| selected.as_deref() == *value),
                "pill",
                ui,
            );
            let own_backdrop = pill.is_none();
            let choices = options
                .into_iter()
                .enumerate()
                .map(|(index, (value, title))| {
                    let chosen = selected.as_deref() == value;
                    div()
                        .id(SharedString::from(format!("{id}-{index}")))
                        .px_2()
                        .h(px(24.))
                        .flex()
                        .items_center()
                        .rounded(px(ui::RADIUS_SM))
                        .text_size(px(theme::TEXT_SM))
                        .cursor_pointer()
                        .when(chosen, |choice| choice.text_color(ui.foreground))
                        .when(chosen && own_backdrop, |choice| choice.bg(ui.list_selected))
                        .when(!chosen, |choice| {
                            choice
                                .text_color(ui.text_muted)
                                .hover(move |style| style.bg(ui.hover).text_color(ui.foreground))
                        })
                        .on_click(cx.listener(move |_, _, _, cx| {
                            settings::update_claude(cx, |claude| {
                                set(claude, value.map(str::to_string))
                            });
                            cx.notify();
                        }))
                        .child(segments.measure(index))
                        .child(title)
                });
            div()
                .flex()
                .flex_col()
                .items_start()
                .gap_1p5()
                .px_3()
                .py_1p5()
                .child(div().text_color(ui.text_muted).child(label))
                .child(
                    div()
                        // The id scopes the pill's glide.
                        .id(id)
                        .flex_none()
                        .relative()
                        .flex()
                        .gap_0p5()
                        .p_0p5()
                        .rounded(px(ui::RADIUS_MD))
                        .bg(ui.input_background)
                        .border_1()
                        .border_color(ui.input_border)
                        .child(segments.measure_origin())
                        .children(pill)
                        .children(choices),
                )
        };
        let defaults_card = card(tr("New Sessions"))
            .child(choice_row(
                "claude-model",
                tr("Model"),
                vec![
                    (None, tr("Default")),
                    (Some("opus"), "Opus"),
                    (Some("sonnet"), "Sonnet"),
                    (Some("haiku"), "Haiku"),
                    (Some("fable"), "Fable"),
                ],
                current.model.clone(),
                |claude, value| claude.model = value,
                cx,
            ))
            .child(choice_row(
                "claude-effort",
                tr("Effort"),
                vec![
                    (None, tr("Default")),
                    (Some("low"), tr("Low")),
                    (Some("medium"), tr("Medium")),
                    (Some("high"), tr("High")),
                    (Some("xhigh"), tr("Extra high")),
                    (Some("max"), tr("Max")),
                ],
                current.effort.clone(),
                |claude, value| claude.effort = value,
                cx,
            ))
            .child(choice_row(
                "claude-mode",
                tr("Permissions"),
                vec![
                    (None, tr("Ask Before Edits")),
                    (Some("acceptEdits"), tr("Accept Edits")),
                    (Some("plan"), tr("Plan")),
                    (Some("auto"), tr("Auto")),
                ],
                current.permission_mode.clone(),
                |claude, value| claude.permission_mode = value,
                cx,
            ))
            .child(
                div()
                    .px_3()
                    .pt_0p5()
                    .pb_2()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.dim)
                    .child(tr(
                        "A session can change them in the chat; these are what a new one starts with.",
                    )),
            );
        let chat_card = card(tr("Chat"))
            .child(switch_row(
                "claude-diff-tabs",
                tr("Show proposed edits in a diff tab"),
                tr("An edit Claude asks about also opens next to your tabs, where hunks can be taken one by one.").into(),
                current.diff_tabs,
                |claude, on| claude.diff_tabs = on,
                cx,
            ))
            .child(switch_row(
                "claude-share-selection",
                tr("Share the editor's selection"),
                tr("The file and the selected lines go along with a message as a mention.").into(),
                current.share_selection,
                |claude, on| claude.share_selection = on,
                cx,
            ))
            .child(switch_row(
                "claude-show-limits",
                tr("Show subscription limits in the status bar"),
                tr("How much of the 5-hour and the weekly limits is used.").into(),
                current.show_limits,
                |claude, on| claude.show_limits = on,
                cx,
            ))
            .child(switch_row(
                "claude-restore-sessions",
                tr("Reopen sessions with the project"),
                tr("The sessions open when the project closed come back, their history read from Claude Code's transcripts.").into(),
                current.restore_sessions,
                |claude, on| claude.restore_sessions = on,
                cx,
            ))
            .child(switch_row(
                "claude-respond-to-commands",
                tr("Claude answers the output of ! commands"),
                trf("A command typed after ! runs in the project's root; its output goes to Claude. On — Claude answers it at once; off — it sees the output with your next message. {0} runs a command the other way.", &[&crate::keymap::shortcut("claude_composer::SendNow", "cmd-enter")]).into(),
                current.answer_commands,
                |claude, on| claude.answer_commands = on,
                cx,
            ))
            .child(switch_row(
                "claude-system-notifications",
                tr("Notify when a session finishes or waits"),
                tr("A macOS notification while Flux isn't the active app.").into(),
                current.system_notifications,
                |claude, on| claude.system_notifications = on,
                cx,
            ));
        let tools_card = card(tr("Flux Tools for Claude"))
            .child(switch_row(
                "claude-flux-tools",
                tr("Give Claude the tools of Flux"),
                tr("Problems the language servers find, definitions, usages and symbols, the open files. Takes effect in new sessions.").into(),
                current.flux_tools,
                |claude, on| claude.flux_tools = on,
                cx,
            ))
            .child(switch_row(
                "claude-report-problems",
                tr("Tell Claude about new problems after its edits"),
                tr("After an edit, the errors the language server finds in the file go back to Claude, so it fixes them at once.").into(),
                current.report_problems,
                |claude, on| claude.report_problems = on,
                cx,
            ));
        let args_card = card(tr("Additional Arguments")).child(
            div()
                .px_3()
                .pb_2()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(self.claude_args.clone())
                .child(
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.dim)
                        .child(tr(
                            "Given to claude when a session starts, after the ones Flux uses: --add-dir, --mcp-config, --settings…",
                        )),
                ),
        );
        let country_card = card(tr("Country Check"))
            .child(switch_row(
                "claude-country-check",
                tr("Block Claude by the country of the IP address"),
                tr("Flux finds out which country the network shows for this Mac and stops Claude while that country is on the list below or can't be found out.").into(),
                current.country_check,
                |claude, on| claude.country_check = on,
                cx,
            ))
            .child(self.render_countries(current.country_check, cx))
            .child(self.render_region_now(cx))
            .child(
                div()
                    .px_3()
                    .pt_1()
                    .pb_2()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.dim)
                    .child(tr(
                        "The stop is at once: running sessions end, and nothing is sent to Claude until the country changes. Flux checks as it starts, every minute (every 10 s while Claude works), when the network changes and before a new request. It asks a neutral service, never Claude's servers.",
                    )),
            );
        let enabled = current.enabled;
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(section_header(
                tr("Claude Code"),
                trf(
                    "Claude Code in Flux: chats in editor tabs, the console on the left ({0}). Flux runs your claude command line tool, signed in with your Claude subscription.",
                    &[&crate::keymap::shortcut("claude::ToggleClaude", "cmd-escape")],
                ),
                ui,
            ))
            .child(
                div()
                    .rounded(px(ui::RADIUS_MD))
                    .border_1()
                    .border_color(ui.island_border)
                    .p_1()
                    .child(switch_row(
                        "claude-enabled",
                        tr("Enable Claude Code"),
                        trf(
                            "The Claude console in the launchpad, {0} and {1}.",
                            &[&crate::keymap::shortcut("claude::ToggleClaude", "cmd-escape"), &crate::keymap::shortcut("claude::AddSelectionToClaude", "alt-cmd-k")],
                        )
                        .into(),
                        enabled,
                        |claude, on| claude.enabled = on,
                        cx,
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .when(!enabled, |cards| cards.opacity(0.6))
                    .child(country_card)
                    .child(cli_card)
                    .child(defaults_card)
                    .child(chat_card)
                    .child(tools_card)
                    .child(args_card),
            )
    }

    /// "Choose…": the `claude` executable from a file panel; Flux looks for it again.
    fn choose_claude(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(tr("Choose").into()),
        });
        let store = self.claude.downgrade();
        window
            .spawn(cx, async move |cx| {
                let Ok(Ok(Some(paths))) = paths.await else {
                    return;
                };
                let Some(path) = paths.into_iter().next() else {
                    return;
                };
                cx.update(|_, cx| {
                    settings::update_claude(cx, |claude| claude.path = Some(path));
                    store.update(cx, |store, cx| store.check(cx)).ok();
                })
                .ok();
            })
            .detach();
    }

    /// Country Check: the field of the blocked countries with its hint, the countries it holds as
    /// chips (flag and name), and what in it isn't a code. `on` — the check is on (dimmed when not).
    fn render_countries(&self, on: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let text = self.countries.read(cx).text();
        let invalid = flux_claude::region::invalid_codes(&text);
        // The list in effect (a typo keeps the previous one).
        let codes: Vec<String> = match region_guard::global(cx) {
            Some(guard) => guard.read(cx).countries().to_vec(),
            None => settings::claude(cx).blocked_countries,
        };
        let chips = codes.iter().map(|code| {
            let name = region_guard::country_name(code);
            div()
                .flex_none()
                .h(px(26.))
                .pl(px(8.))
                .pr(px(10.))
                .flex()
                .items_center()
                .gap_1p5()
                .rounded(px(13.))
                .bg(ui.input_background)
                .border_1()
                .border_color(ui.input_border)
                .text_size(px(theme::TEXT_SM))
                .child(region_status::flag(code, 14.))
                .child(
                    div()
                        .text_color(ui.foreground)
                        .child(name.clone().unwrap_or_else(|| code.clone())),
                )
                .when(name.is_some(), |chip| {
                    chip.child(div().text_color(ui.dim).child(code.clone()))
                })
        });
        let note = codes.is_empty().then(|| {
            div()
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.warning)
                .child(tr(
                    "No countries on the list: Claude is blocked only when the country can't be found out.",
                ))
        });
        let typos = (!invalid.is_empty()).then(|| {
            div()
                .flex()
                .gap_1p5()
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.warning)
                .child(
                    div()
                        .flex_none()
                        .h(px(16.))
                        .flex()
                        .items_center()
                        .child(icon(IconName::Warning, ui.warning).size(px(13.))),
                )
                .child(div().flex_1().min_w_0().child(trf(
                    "Not country codes: {0}. The list stays as it was until they are fixed.",
                    &[&invalid.join(", ")],
                )))
        });
        div()
            .px_3()
            .pt_1()
            .pb_2()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_color(ui.text_muted)
                    .child(tr("Blocked countries")),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(self.countries.clone())
                    .child(
                        div()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.dim)
                            .child(tr(
                                "Two-letter country codes, separated by commas: RU, BY, IR.",
                            )),
                    ),
            )
            .when(!codes.is_empty(), |list| {
                list.child(div().pt_0p5().flex().flex_wrap().gap_1p5().children(chips))
            })
            .children(note)
            .children(typos)
            // It depends on the Country Check switch: dims and comes back with a fade.
            .with_transition(
                "countries-dim",
                if on { 1. } else { 0.6 },
                motion::FADE,
                |list, opacity| list.opacity(opacity),
            )
    }

    /// Country Check: where the network sees this Mac now, what that means for Claude, when it was
    /// checked, and Check Again.
    fn render_region_now(&self, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let verdict = region_guard::verdict(cx);
        let asking = region_guard::global(cx).is_some_and(|guard| guard.read(cx).is_checking());
        let place = match &verdict {
            Verdict::Off => tr("Not checked").to_string(),
            Verdict::Checking => tr("Checking the country of the IP address…").to_string(),
            Verdict::Unknown(_) => tr("The country of the IP address is unknown").to_string(),
            verdict => region_status::place(verdict).unwrap_or_default(),
        };
        let checked = match (region_status::checked(&verdict), asking) {
            (_, true) if verdict != Verdict::Checking => Some(tr("checking now…").to_string()),
            (Some(_), _) => verdict
                .checked_at()
                .map(|at| trf("checked {0}", &[&region_status::ago(at)])),
            (None, _) => None,
        };
        let mark =
            region_status::mark(&verdict, 20., "settings-region-pulse", ui).unwrap_or_else(|| {
                icon(IconName::Globe, ui.dim)
                    .size(px(18.))
                    .into_any_element()
            });
        div()
            .mx_3()
            .mt_1()
            .mb_1p5()
            .px_3()
            .py(px(10.))
            .flex()
            .items_center()
            .gap_3()
            .rounded(px(ui::RADIUS_MD))
            .bg(ui.input_background)
            .border_1()
            .border_color(if verdict.blocks() {
                UiColors::tint(ui.error, 0.4)
            } else {
                ui.input_border
            })
            .child(
                div()
                    .flex_none()
                    .w(px(24.))
                    .flex()
                    .justify_center()
                    .child(mark),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(div().text_color(ui.foreground).child(place))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_x_1()
                            .text_size(px(theme::TEXT_SM))
                            .child(
                                div()
                                    .text_color(region_status::tone(&verdict, ui))
                                    .child(region_status::state(&verdict)),
                            )
                            .children(checked.map(|checked| {
                                div().text_color(ui.dim).child(format!("· {checked}"))
                            })),
                    ),
            )
            .when(verdict != Verdict::Off, |row| {
                row.child(
                    ui::available(
                        ui::text_button(
                            "claude-country-check-again",
                            if asking {
                                tr("Checking…")
                            } else {
                                tr("Check Again")
                            },
                            false,
                            ui,
                        ),
                        !asking,
                    )
                    .when(!asking, |button| {
                        button.on_click(cx.listener(|_, _, _, cx| {
                            region_guard::check_again(cx);
                            cx.notify();
                        }))
                    }),
                )
            })
    }

    /// About: the logo with the name, version, and description; then the facts in rows — the build,
    /// this Mac — and the buttons of a report.
    fn render_about(&self, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let version = env!("CARGO_PKG_VERSION");
        let machine = feedback::machine();
        let identity = div()
            .flex()
            .items_center()
            .gap_4()
            .child(logo_tile(ABOUT_LOGO_SIZE, ui))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(px(theme::TEXT_XL))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(ui.foreground)
                                    .child("Flux"),
                            )
                            .child(ui::badge(format!("v{version}"), ui.accent_text))
                            // A beta says so (part 10.8).
                            .when(
                                flux_update::Channel::of(&flux_update::current_version())
                                    == flux_update::Channel::Beta,
                                |row| row.child(ui::badge(tr("Beta"), ui.warning)),
                            ),
                    )
                    .child(
                        div()
                            .text_color(ui.text_muted)
                            .child(tr("A fast, minimal code editor")),
                    ),
            );
        let repository = div()
            .id("about-repository")
            .text_color(ui.accent_text)
            .cursor_pointer()
            .hover(|style| style.underline())
            .on_click(|_, _, cx| cx.open_url(&format!("https://{REPOSITORY}")))
            .child(REPOSITORY)
            .into_any_element();
        // The system's name labels its version: "macOS" — "26.1 (25B78)", "Windows" — "11 24H2
        // (26100.4061)".
        let version = machine
            .macos
            .strip_prefix(feedback::SYSTEM_NAME)
            .map(str::trim_start)
            .filter(|version| !version.is_empty())
            .unwrap_or(&machine.macos)
            .to_string();
        let facts = [
            (tr("Version"), plain(machine.version.clone(), ui)),
            (feedback::SYSTEM_NAME, plain(version, ui)),
            (tr("Computer"), plain(feedback::computer(), ui)),
            (tr("License"), plain(env!("CARGO_PKG_LICENSE"), ui)),
            (tr("Developer"), plain(DEVELOPER, ui)),
            (tr("Source code"), repository),
        ];
        // Copy last: «Copied» is wider and moves nothing.
        let buttons = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                ui::text_button("about-report-bug", tr("Report a Bug…"), false, ui)
                    .on_click(|_, _, cx| feedback::report_bug(None, cx)),
            )
            .child(
                ui::copy_button("about-copy", &self.about_copied, ui)
                    .tooltip(ui::tooltip(
                        tr("The version, macOS, the computer and the plugins — for a report"),
                        None,
                    ))
                    .on_click(cx.listener(|this, _, _, cx| this.copy_about(cx))),
            );
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(identity)
            .child(ui::divider(ui))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2p5()
                    .children(facts.into_iter().map(|(label, value)| {
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(ABOUT_LABEL_WIDTH))
                                    .text_color(ui.dim)
                                    .child(label),
                            )
                            .child(value)
                    })),
            )
            .child(buttons)
    }

    /// About → Copy: the facts for a report, with this window's plugins.
    fn copy_about(&mut self, cx: &mut Context<Self>) {
        let text = feedback::system_info_with(&self.plugins, cx).to_text();
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.about_copied.now(cx);
    }
}

/// The logo on a tile like the app icon: dark glass, a highlight, the colored spark, and a soft
/// accent glow.
fn logo_tile(size: f32, ui: UiColors) -> Div {
    let radius = size * 0.25;
    div()
        .relative()
        .flex_none()
        .size(px(size))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(radius))
        .bg(linear_gradient(
            180.,
            linear_color_stop(ui.elevated, 0.),
            linear_color_stop(UiColors::tint(ui.island, 1.), 1.),
        ))
        .border_1()
        .border_color(ui.elevated_border)
        .shadow(vec![
            BoxShadow {
                color: UiColors::tint(ui.accent, 0.30),
                offset: point(px(0.), px(size * 0.13)),
                blur_radius: px(size * 0.42),
                spread_radius: px(-6.),
            },
            BoxShadow {
                color: UiColors::tint(ui.shadow, 0.3),
                offset: point(px(0.), px(2.)),
                blur_radius: px(6.),
                spread_radius: px(0.),
            },
        ])
        .child(ui::sheen(ui, radius))
        .child(img(icons::LOGO).size(px(size * 0.68)))
}

/// The rows of Settings → Language Servers: the servers of the turned-on language plugins, each
/// with its plugin's name.
fn server_rows(plugins: &Entity<PluginStore>, cx: &App) -> Vec<ServerRow> {
    let store = plugins.read(cx);
    crate::contributions::servers(cx)
        .into_iter()
        .map(|config| {
            let plugin = crate::contributions::server_plugin(&config.name, cx)
                .map(|id| match store.plugin(&id) {
                    Some(plugin) => plugin.name().to_string(),
                    None => id,
                })
                .unwrap_or_default();
            ServerRow {
                effective: crate::lsp_settings::effective(&config, cx),
                config,
                plugin: plugin.into(),
                source: None,
                installable: Ok(()),
            }
        })
        .collect()
}

/// A section's title and its description.
fn section_header(title: &'static str, description: impl Into<SharedString>, ui: UiColors) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(px(theme::TEXT_LG))
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(div().text_color(ui.text_muted).child(description.into()))
}

/// A value in About: plain text that can be selected by eye, not edited.
fn plain(text: impl Into<SharedString>, ui: UiColors) -> AnyElement {
    div()
        .text_color(ui.foreground)
        .child(text.into())
        .into_any_element()
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        // The Plugins page scrolls its list and its details apart: it takes the whole height.
        let content = match self.section.clone() {
            Section::Theme | Section::Interface | Section::FileIcons | Section::Language => {
                Some(match &self.appearance {
                    Some(page) => page.clone().into_any_element(),
                    None => div().into_any_element(),
                })
            }
            Section::System => Some(self.render_system(cx).into_any_element()),
            Section::Editor => Some(match &self.editor {
                Some(page) => page.clone().into_any_element(),
                None => div().into_any_element(),
            }),
            Section::Updates => Some(match &self.updates {
                Some(page) => page.clone().into_any_element(),
                None => div().into_any_element(),
            }),
            Section::LanguageServers => Some(self.render_language_servers(cx).into_any_element()),
            Section::VersionControl => Some(self.render_version_control(cx).into_any_element()),
            Section::Notifications => Some(self.render_notifications(cx).into_any_element()),
            Section::Claude => Some(self.render_claude(cx).into_any_element()),
            Section::Plugins => None,
            Section::Plugin(id) => Some(match self.plugin_pages.get(&id) {
                Some(page) => page.clone().into_any_element(),
                None => div().into_any_element(),
            }),
            Section::About => Some(self.render_about(cx).into_any_element()),
        };
        // A page coming in fades in rising a little, once: keyed by the section, so the page shown
        // doesn't play it again when it redraws.
        let appear = motion::Appear::slide(0., PAGE_RISE).curve(motion::POP);
        let page = SharedString::from(format!("settings-page-{}", self.section.key()));
        let content = match content {
            // Each section scrolls on its own: a page comes in at its top, not where the last
            // one was scrolled to.
            Some(content) => div()
                .id(SharedString::from(format!(
                    "settings-content-{}",
                    self.section.key()
                )))
                .flex_1()
                .min_w_0()
                .overflow_y_scroll()
                .p_4()
                .child(appear.apply(page, div().child(content)))
                .into_any_element(),
            None => appear.apply(
                page,
                div()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .children(self.manager.clone()),
            ),
        };
        let viewport = window.viewport_size();
        let width = WIDTH.min(f32::from(viewport.width) - 2. * WINDOW_MARGIN);
        let height = ui::dialog_height(HEIGHT, window);
        ui::popover(ui)
            .key_context("Settings")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(DismissEvent)))
            .on_action(
                cx.listener(|this, _: &About, window, cx| this.select(Section::About, window, cx)),
            )
            .w(px(width))
            .h(px(height))
            .flex()
            .flex_col()
            .text_size(px(theme::TEXT_MD))
            .child(
                div()
                    .flex_none()
                    .h(px(44.))
                    .px_4()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(ui.divider)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(icon(IconName::Settings, ui.text_muted).size(px(15.)))
                            .child(tr("Settings")),
                    )
                    .child(
                        ui::icon_button("settings-close", IconName::Close, ui)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.render_sidebar(cx))
                    .child(content),
            )
    }
}

/// Where installed servers live, for the description.
fn dirs_label() -> String {
    std::env::var_os("FLUX_SERVERS_DIR")
        .filter(|dir| !dir.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| flux_platform::paths::support_dir().map(|dir| dir.join("servers")))
        .map(|dir| tilde(&dir))
        .unwrap_or_default()
}

/// Shown on the launchpad: whether Settings are open in this window.
pub fn is_open(workspace: &Workspace) -> bool {
    workspace.modal_is::<SettingsView>()
}
