//! Settings (⌘, or the gear at the bottom of the launchpad): an overlay window with sections on the
//! left.
//! - Language Servers: automatic installation on or off, and every server Flux knows with where it
//!   comes from; those Flux installed can be updated and deleted, missing ones installed.
//! - Version Control: how Update Project (⌘T) brings incoming commits into the current branch —
//!   merge, rebase, or ask every time (the question's "Don't show again" sets it).
//! - Notifications: Do Not Disturb, and for every group (plugins' too) how its notifications show —
//!   a card, a sticky card, the journal only, or nothing (JetBrains: Settings → Notifications).
//! - Plugins: the plugin manager ([`crate::plugin_manager`]); under it, a page of every plugin that
//!   is on and has settings ([`crate::plugin_settings`]).
//! - About: the version, the license, the developer, the source code. «About Flux» in the app menu
//!   ([`About`]) opens Settings on this section.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;

use flux_lsp::config::{self, ServerConfig};
use flux_lsp::install::{self, Source};
use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    AnyElement, App, AppContext as _, BoxShadow, Context, DismissEvent, Div, Entity, EventEmitter,
    FocusHandle, Focusable, FontWeight, KeyBinding, Render, SharedString, Subscription, Task,
    WeakEntity, Window, actions, div, img, linear_color_stop, linear_gradient, point, prelude::*,
    px,
};

use crate::i18n::{tr, trf};
use crate::icons::{self, IconName, file_icon, icon};
use crate::lsp::LspStore;
use crate::notification_center::{self, Display};
use crate::plugin_manager::{PluginManager, PluginManagerEvent};
use crate::plugin_settings::PluginSettingsPage;
use crate::plugins::{PluginStatus, PluginStore};
use crate::settings::{self, UpdatePreference};
use crate::theme::{self, Theme, UiColors};
use crate::ui;
use crate::workspace::{Workspace, tilde};

actions!(settings, [Toggle, Dismiss, About]);

/// The window's size; a small Flux window gets a smaller one, with a margin around it.
const WIDTH: f32 = 880.;
const HEIGHT: f32 = 600.;
/// The margin between Settings and the edges of a small window; the top is under the title bar.
const WINDOW_MARGIN: f32 = 16.;
const WINDOW_TOP: f32 = ui::TITLE_BAR_HEIGHT + 32.;
const SIDEBAR_WIDTH: f32 = 180.;
/// About: the logo tile and the column of row labels.
const ABOUT_LOGO_SIZE: f32 = 64.;
const ABOUT_LABEL_WIDTH: f32 = 120.;

/// The developer and the repository, for About.
const DEVELOPER: &str = "Egor Ageev";
const REPOSITORY: &str = "github.com/egor4geev/flux";

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-,", Toggle, Some("Workspace")),
        KeyBinding::new("escape", Dismiss, Some("Settings")),
    ]);
    // Open Settings switch to About themselves (they have focus); otherwise the action reaches the
    // app, and the active window opens them on About.
    cx.on_action(about);
}

/// A section of Settings, listed on the left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Section {
    LanguageServers,
    VersionControl,
    Notifications,
    /// The plugin manager.
    Plugins,
    /// A plugin's settings, by its id: listed under Plugins while the plugin is on.
    Plugin(SharedString),
    About,
}

impl Section {
    /// The sections above the plugins' pages; About goes after them.
    const BUILT_IN: [Section; 4] = [
        Section::LanguageServers,
        Section::VersionControl,
        Section::Notifications,
        Section::Plugins,
    ];

    fn label(&self) -> &'static str {
        match self {
            Section::LanguageServers => tr("Language Servers"),
            Section::VersionControl => tr("Version Control"),
            Section::Notifications => tr("Notifications"),
            Section::Plugins | Section::Plugin(_) => tr("Plugins"),
            Section::About => tr("About"),
        }
    }

    fn icon(&self) -> IconName {
        match self {
            Section::LanguageServers => IconName::Command,
            Section::VersionControl => IconName::Branch,
            Section::Notifications => IconName::Bell,
            Section::Plugins | Section::Plugin(_) => IconName::Puzzle,
            Section::About => IconName::Info,
        }
    }

    /// A stable key: the element ids of the sidebar.
    fn key(&self) -> SharedString {
        match self {
            Section::LanguageServers => "language-servers".into(),
            Section::VersionControl => "version-control".into(),
            Section::Notifications => "notifications".into(),
            Section::Plugins => "plugins".into(),
            Section::Plugin(id) => format!("plugin-{id}").into(),
            Section::About => "about".into(),
        }
    }
}

/// Opens Settings, or closes them if open (the gear, ⌘,).
pub fn toggle(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    #[cfg(feature = "scenario")]
    let section = scenario_section().unwrap_or(Section::LanguageServers);
    #[cfg(not(feature = "scenario"))]
    let section = Section::LanguageServers;
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
}

/// With the `scenario` feature, `FLUX_SCENARIO_SETTINGS=plugins` (or `plugin:<id>`) opens Settings
/// on that section: a scenario can't click the sidebar.
#[cfg(feature = "scenario")]
fn scenario_section() -> Option<Section> {
    let section = std::env::var("FLUX_SCENARIO_SETTINGS").ok()?;
    Some(match section.as_str() {
        "plugins" => Section::Plugins,
        "notifications" => Section::Notifications,
        "version-control" => Section::VersionControl,
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
    servers: Vec<ServerRow>,
    /// Operations in progress or failed, by server name.
    jobs: HashMap<String, Job>,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

struct ServerRow {
    config: ServerConfig,
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
        let servers = config::default_servers()
            .into_iter()
            .map(|config| ServerRow {
                config,
                source: None,
                installable: Ok(()),
            })
            .collect();
        // A plugin turned off takes its page with it.
        let subscriptions = vec![cx.observe(&handles.plugins, |this, _, cx| {
            if let Section::Plugin(id) = &this.section
                && !this.plugin_pages_listed(cx).contains(id)
            {
                this.section = Section::Plugins;
            }
            cx.notify()
        })];
        let mut view = Self {
            focus_handle: cx.focus_handle(),
            section: Section::LanguageServers,
            store: handles.lsp,
            plugins: handles.plugins,
            workspace: handles.workspace,
            manager: None,
            initial_plugin: plugin,
            plugin_pages: HashMap::new(),
            servers,
            jobs: HashMap::new(),
            _tasks: Vec::new(),
            _subscriptions: subscriptions,
        };
        view.refresh(cx);
        view.select(section, window, cx);
        view
    }

    /// Where each server comes from: file checks (and `rustup which`) in the background.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let configs: Vec<ServerConfig> =
            self.servers.iter().map(|row| row.config.clone()).collect();
        let task = cx.spawn(async move |this, cx| {
            let found = cx
                .background_spawn(async move {
                    configs
                        .iter()
                        .map(|config| (install::source(config), install::check(config)))
                        .collect::<Vec<_>>()
                })
                .await;
            this.update(cx, |this, cx| {
                for (row, (source, installable)) in this.servers.iter_mut().zip(found) {
                    row.source = Some(source);
                    row.installable = installable;
                }
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
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
        let languages = config
            .extensions
            .iter()
            .map(|extension| format!(".{extension}"))
            .collect::<Vec<_>>()
            .join(" ");
        let file = file_icon(
            &format!("x.{}", config.extensions.first().map_or("", String::as_str)),
            &ui,
        );
        let job = self.jobs.get(&config.name);
        let (status, status_color): (String, _) = match (job, &row.source) {
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
            Some(Source::Missing) if row.installable.is_ok() => vec![
                ui::text_button(("install", index), tr("Install"), false, ui)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.run(index, Operation::Install, cx)),
                    )
                    .into_any_element(),
            ],
            _ => Vec::new(),
        };
        div()
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
                                    .child(config.name.clone()),
                            )
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
            .child(div().flex_none().flex().gap_1p5().children(buttons))
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
                    let page = cx.new(|cx| PluginSettingsPage::new(id.clone(), plugins, window, cx));
                    self.plugin_pages.insert(id, page);
                }
                window.focus(&self.focus_handle);
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
    /// indented under Plugins, as the settings tree of JetBrains IDEs.
    fn render_sidebar(&self, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let store = self.plugins.read(cx);
        let plugin_pages: Vec<(Section, SharedString)> = self
            .plugin_pages_listed(cx)
            .into_iter()
            .filter_map(|id| {
                let plugin = store.plugin(&id)?;
                let name = plugin.tr(&plugin.entry.manifest.name).to_string();
                Some((Section::Plugin(id), SharedString::from(name)))
            })
            .collect();
        let row = |section: Section, label: SharedString, nested: bool, cx: &mut Context<Self>| {
            let selected = section == self.section;
            let icon_name = section.icon();
            div()
                .id(SharedString::from(format!("settings-section-{}", section.key())))
                .h(px(30.))
                .px_2()
                .when(nested, |row| row.pl(px(30.)))
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
                .when(selected, |row| row.bg(ui.list_selected))
                .when(!selected, |row| {
                    row.hover(move |style| style.bg(ui.hover).text_color(ui.foreground))
                })
                .on_click(
                    cx.listener(move |this, _, window, cx| {
                        this.select(section.clone(), window, cx)
                    }),
                )
                .when(!nested, |row| {
                    row.child(
                        icon(icon_name, if selected { ui.accent_text } else { ui.dim })
                            .size(px(14.)),
                    )
                })
                .child(div().min_w_0().truncate().child(label))
        };
        let mut rows: Vec<AnyElement> = Section::BUILT_IN
            .into_iter()
            .map(|section| {
                let label = SharedString::from(section.label());
                row(section, label, false, cx).into_any_element()
            })
            .collect();
        rows.extend(
            plugin_pages
                .into_iter()
                .map(|(section, label)| row(section, label, true, cx).into_any_element()),
        );
        rows.push(row(Section::About, tr("About").into(), false, cx).into_any_element());
        div()
            .flex_none()
            .w(px(SIDEBAR_WIDTH))
            .p_2()
            .flex()
            .flex_col()
            .gap_0p5()
            .border_r_1()
            .border_color(ui.divider)
            .children(rows)
    }

    fn render_language_servers(&self, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let auto = settings::auto_install_servers(cx);
        let servers_dir = dirs_label();
        let rows: Vec<_> = (0..self.servers.len())
            .map(|index| self.render_server(index, cx))
            .collect();
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
                tr(
                    "Git: how Update Project (⌘T) brings the commits of the upstream into the current branch. Local changes are stashed for the update and come back after it.",
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
                let choices = Display::ALL.into_iter().map(|display| {
                    let key = key.clone();
                    let selected = display == current;
                    div()
                        .id(SharedString::from(format!("display-{index}-{}", display.key())))
                        .px_2()
                        .h(px(24.))
                        .flex()
                        .items_center()
                        .rounded(px(ui::RADIUS_SM))
                        .text_size(px(theme::TEXT_SM))
                        .cursor_pointer()
                        .when(selected, |choice| {
                            choice.bg(ui.list_selected).text_color(ui.foreground)
                        })
                        .when(!selected, |choice| {
                            choice
                                .text_color(ui.text_muted)
                                .hover(move |style| style.bg(ui.hover).text_color(ui.foreground))
                        })
                        .on_click(cx.listener(move |_, _, _, cx| {
                            settings::set_notification_display(&key, display, cx);
                            cx.notify();
                        }))
                        .child(display.label())
                });
                div()
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
                            .flex()
                            .gap_0p5()
                            .p_0p5()
                            .rounded(px(ui::RADIUS_MD))
                            .bg(ui.input_background)
                            .border_1()
                            .border_color(ui.input_border)
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

    /// About: the logo with the name, version, and description; then the facts in rows.
    fn render_about(&self, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let version = env!("CARGO_PKG_VERSION");
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
                            .child(ui::badge(format!("v{version}"), ui.accent_text)),
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
        let facts = [
            (tr("Version"), plain(version, ui)),
            (tr("License"), plain(env!("CARGO_PKG_LICENSE"), ui)),
            (tr("Developer"), plain(DEVELOPER, ui)),
            (tr("Source code"), repository),
        ];
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

/// A section's title and its description.
fn section_header(title: &'static str, description: &'static str, ui: UiColors) -> Div {
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
        .child(div().text_color(ui.text_muted).child(description))
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
            Section::LanguageServers => Some(self.render_language_servers(cx).into_any_element()),
            Section::VersionControl => Some(self.render_version_control(cx).into_any_element()),
            Section::Notifications => Some(self.render_notifications(cx).into_any_element()),
            Section::Plugins => None,
            Section::Plugin(id) => Some(match self.plugin_pages.get(&id) {
                Some(page) => page.clone().into_any_element(),
                None => div().into_any_element(),
            }),
            Section::About => Some(self.render_about(cx).into_any_element()),
        };
        let content = match content {
            Some(content) => div()
                .id("settings-content")
                .flex_1()
                .min_w_0()
                .overflow_y_scroll()
                .p_4()
                .child(content)
                .into_any_element(),
            None => div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .children(self.manager.clone())
                .into_any_element(),
        };
        let viewport = window.viewport_size();
        let width = WIDTH.min(f32::from(viewport.width) - 2. * WINDOW_MARGIN);
        let height = HEIGHT.min(f32::from(viewport.height) - WINDOW_TOP - WINDOW_MARGIN);
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
        .or_else(|| {
            std::env::var_os("HOME").map(|home| {
                std::path::PathBuf::from(home).join("Library/Application Support/flux/servers")
            })
        })
        .map(|dir| tilde(&dir))
        .unwrap_or_default()
}

/// Shown on the launchpad: whether Settings are open in this window.
pub fn is_open(workspace: &Workspace) -> bool {
    workspace.modal_is::<SettingsView>()
}
