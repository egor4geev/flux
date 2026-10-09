//! Settings → Plugins (stage 8, ADR-029), as Plugins in JetBrains IDEs: the installed plugins —
//! bundled, downloaded, under development — with their details (description, permissions, what
//! they add, the log), turning them on and off without a restart, removing them, and «Install
//! Plugin from Disk…» with a question about the plugin's permissions.
//!
//! The page is a list on the left (a search field, the plugins in groups, a switch on each) and
//! the selected plugin on the right: who made it, its buttons, and tabs — Overview, Permissions,
//! Contributions, Log. The gear menu installs a plugin from a folder (under development: Flux
//! builds it with cargo and reloads it when it changes) or an archive, and reloads the plugins
//! under development. The catalog (Marketplace) comes in 8.2.

use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use flux_plugin::install::{Candidate, CandidateKind};
use flux_plugin::log::{Level, LogLine, PluginLog};
use flux_plugin::manifest::{Manifest, Permissions, ProjectAccess};
use flux_plugin::registry::{PluginEntry, PluginSource, ScanError};
use gpui::{
    AnyElement, App, AppContext as _, AsyncWindowContext, ClickEvent, Context, DismissEvent, Div,
    Entity, EventEmitter, FocusHandle, Focusable, FontWeight, Hsla, KeyBinding, Keystroke,
    MouseButton, MouseDownEvent, PathPromptOptions, Pixels, Point, Render, ScrollStrategy,
    SharedString, Subscription, UniformListScrollHandle, WeakEntity, Window, actions, div,
    prelude::*, px, svg, uniform_list,
};

use crate::command_palette::keystroke_label;
use crate::context_menu::ContextMenu;
use crate::dialog::Dialog;
use crate::i18n::{lang_code, tr, trf, trn};
use crate::icons::{self, IconName, icon};
use crate::input::{InputEvent, TextInput};
use crate::plugins::{self, PluginStatus, PluginStore};
use crate::settings_view::{self, Section};
use crate::theme::{self, Theme, UiColors};
use crate::ui;
use crate::workspace::{Workspace, tilde};

actions!(plugin_manager, [SelectNext, SelectPrevious]);

/// The list of plugins; the details take the rest of the page.
const LIST_WIDTH: f32 = 248.;
/// The plugin's icon in the list, and in the details.
const LIST_ICON: f32 = 18.;
const DETAILS_ICON: f32 = 30.;
/// The tabs of the details, as the tool window tabs of JetBrains IDEs.
const TABS_HEIGHT: f32 = 34.;
/// A line of the log, in the code font.
const LOG_LINE_HEIGHT: f32 = 18.;

pub fn init(cx: &mut App) {
    // ↑ / ↓ go through the list from its search field.
    let context = Some("PluginManager");
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("up", SelectPrevious, context),
    ]);
}

/// Opens Settings on Plugins: the palette's «Plugins…», a notification about a plugin.
pub fn open(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    settings_view::open(workspace, Section::Plugins, window, cx);
}

/// «Install Plugin from Disk…»: picks a folder or an archive, asks about the plugin's
/// permissions, installs it, and shows it in Settings → Plugins.
pub fn install_from_disk(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let _ = workspace;
    run_install(cx.weak_entity(), None, window, cx);
}

/// The install flow: the file panel, reading the plugin, the question, installing. `manager` — the
/// Plugins page the flow started from: it then selects the plugin; without it, Settings open on
/// it.
fn run_install(
    workspace: WeakEntity<Workspace>,
    manager: Option<WeakEntity<PluginManager>>,
    window: &mut Window,
    cx: &mut App,
) {
    // A scenario can't drive the system file panel: it names the plugin in the environment.
    #[cfg(feature = "scenario")]
    if let Some(path) = std::env::var_os("FLUX_SCENARIO_INSTALL_PATH") {
        let path = PathBuf::from(path);
        window
            .spawn(cx, async move |cx| {
                install_path(path, workspace, manager, cx).await
            })
            .detach();
        return;
    }
    let paths = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: true,
        multiple: false,
        prompt: Some(tr("Install").into()),
    });
    window
        .spawn(cx, async move |cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            install_path(path, workspace, manager, cx).await
        })
        .detach();
}

/// Reads the plugin at `path`, asks the user, installs it.
async fn install_path(
    path: PathBuf,
    workspace: WeakEntity<Workspace>,
    manager: Option<WeakEntity<PluginManager>>,
    cx: &mut AsyncWindowContext,
) {
    let inspected = cx
        .background_spawn(async move { flux_plugin::install::inspect(&path) })
        .await;
    let candidate = match inspected {
        Ok(candidate) => candidate,
        Err(reason) => {
            Dialog::warning(tr("Can't install the plugin"))
                .message(reason)
                .primary(tr("OK"))
                .show_async(cx)
                .await;
            return;
        }
    };
    let name = translated(&candidate.entry, &candidate.entry.manifest.name).to_string();
    if let Some(problem) = candidate.entry.problem.clone() {
        Dialog::warning(trf("Can't install “{0}”", &[&name]))
            .message(problem)
            .primary(tr("OK"))
            .show_async(cx)
            .await;
        return;
    }
    if install_question(&candidate).show_async(cx).await != Some(INSTALL) {
        return;
    }
    let id = SharedString::from(candidate.entry.id().to_string());
    let installed = workspace.update(cx, |workspace, cx| {
        workspace
            .plugins
            .update(cx, |store, cx| store.install(candidate, cx))
    });
    match installed {
        // The window is gone.
        Err(_) => {}
        Ok(Err(reason)) => {
            Dialog::warning(trf("Couldn't install “{0}”", &[&name]))
                .message(reason)
                .primary(tr("OK"))
                .show_async(cx)
                .await;
        }
        Ok(Ok(())) => {
            let shown = manager.is_some_and(|manager| {
                manager
                    .update(cx, |manager, cx| manager.reveal(id.clone(), cx))
                    .is_ok()
            });
            if !shown {
                workspace
                    .update_in(cx, |workspace, window, cx| {
                        settings_view::open_plugin(workspace, id, window, cx)
                    })
                    .ok();
            }
        }
    }
}

/// The index of Install in [`install_question`].
const INSTALL: usize = 0;

/// The question before installing: who made the plugin and what it will be able to do.
fn install_question(candidate: &Candidate) -> Dialog {
    let entry = &candidate.entry;
    let manifest = &entry.manifest;
    let mut paragraphs: Vec<String> = Vec::new();
    if !manifest.authors.is_empty() {
        paragraphs.push(trf("By {0}", &[&manifest.authors.join(", ")]));
    }
    let permissions = permission_lines(&manifest.permissions);
    if permissions.is_empty() {
        paragraphs.push(tr("The plugin asks for no special permissions.").to_string());
    } else {
        paragraphs.push(tr("The plugin will be able to:").to_string());
        paragraphs.extend(
            permissions
                .into_iter()
                .map(|(_, line)| format!("•  {line}")),
        );
    }
    if candidate.kind == CandidateKind::Folder {
        paragraphs.push(
            tr("It will be linked as a plugin under development: Flux builds it with cargo and reloads it when it changes.")
                .to_string(),
        );
    }
    let name = translated(entry, &manifest.name);
    Dialog::info(trf("Install “{0}” {1}?", &[&name, &manifest.version]))
        .message(paragraphs.join("\n\n"))
        .primary(tr("Install"))
        .cancel(tr("Cancel"))
}

/// What the permissions let a plugin do, in plain words: an icon and a line each.
fn permission_lines(permissions: &Permissions) -> Vec<(IconName, &'static str)> {
    let mut lines = Vec::new();
    match permissions.project {
        ProjectAccess::None => {}
        ProjectAccess::Read => lines.push((
            IconName::FolderOpen,
            tr("Read and search the files of the project"),
        )),
        ProjectAccess::Write => lines.push((
            IconName::Pencil,
            tr("Read, search and change the files of the project"),
        )),
    }
    lines
}

/// A string of the plugin's manifest in the interface language.
fn translated<'a>(entry: &'a PluginEntry, text: &'a str) -> &'a str {
    entry.translate(lang_code(), text)
}

/// A plugin as the page draws it: taken from the store, so drawing doesn't hold it.
#[derive(Clone)]
struct Shown {
    id: SharedString,
    entry: Arc<PluginEntry>,
    status: PluginStatus,
    log: PluginLog,
}

impl Shown {
    fn manifest(&self) -> &Manifest {
        &self.entry.manifest
    }

    fn name(&self) -> String {
        translated(&self.entry, &self.entry.manifest.name).to_string()
    }

    /// On: running, starting, or stopped by itself (the user didn't turn it off).
    fn on(&self) -> bool {
        is_on(&self.status)
    }

    fn available(&self) -> bool {
        !matches!(self.status, PluginStatus::Unavailable(_))
    }

    /// Whether the search query finds it: the name (as shown or in English), the id, the
    /// description, the authors.
    fn matches(&self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return true;
        }
        let manifest = self.manifest();
        [
            self.name(),
            manifest.name.clone(),
            manifest.id.clone(),
            translated(&self.entry, &manifest.description).to_string(),
            manifest.authors.join(" "),
        ]
        .iter()
        .any(|text| text.to_lowercase().contains(&query))
    }
}

fn is_on(status: &PluginStatus) -> bool {
    matches!(
        status,
        PluginStatus::Running | PluginStatus::Starting | PluginStatus::Stopped { .. }
    )
}

/// The groups of the list, in its order: what the author works on first.
const GROUPS: [PluginSource; 3] = [
    PluginSource::Dev,
    PluginSource::Installed,
    PluginSource::Bundled,
];

/// A folder that looks like a plugin but can't be read: its name, why, the full path on hover.
fn broken_row(index: usize, error: ScanError, ui: UiColors) -> impl IntoElement {
    let name = error
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| error.path.to_string_lossy().into_owned());
    let path: SharedString = error.path.to_string_lossy().into_owned().into();
    div()
        .id(("broken-plugin", index))
        .px_2()
        .py_1p5()
        .flex()
        .gap_2()
        .rounded(px(ui::RADIUS_SM))
        .tooltip(ui::tooltip(path, None))
        .child(icon(IconName::Error, ui.error).size(px(14.)).mt_0p5())
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().truncate().text_color(ui.foreground).child(name))
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(ui.error)
                        .child(error.message),
                ),
        )
}

fn group_label(source: PluginSource) -> &'static str {
    match source {
        PluginSource::Dev => tr("Development"),
        PluginSource::Installed => tr("Downloaded"),
        PluginSource::Bundled => tr("Bundled"),
    }
}

/// The tabs of a plugin's details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Overview,
    Permissions,
    Contributions,
    Log,
}

impl Tab {
    const ALL: [Tab; 4] = [
        Tab::Overview,
        Tab::Permissions,
        Tab::Contributions,
        Tab::Log,
    ];

    fn label(self) -> &'static str {
        match self {
            Tab::Overview => tr("Overview"),
            Tab::Permissions => tr("Permissions"),
            Tab::Contributions => tr("Contributions"),
            Tab::Log => tr("Log"),
        }
    }
}

pub enum PluginManagerEvent {
    /// «Settings» of a plugin: its page in Settings.
    OpenSettings(SharedString),
}

/// The Plugins page of Settings.
pub struct PluginManager {
    plugins: Entity<PluginStore>,
    workspace: WeakEntity<Workspace>,
    search: Entity<TextInput>,
    /// The plugin shown on the right, by id.
    selected: Option<SharedString>,
    tab: Tab,
    /// The lines of the shown log, for its list; new ones scroll it to the bottom.
    log_lines: Vec<LogLine>,
    log_scroll: UniformListScrollHandle,
    /// The gear menu.
    menu: Option<OpenMenu>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

struct OpenMenu {
    menu: Entity<ContextMenu>,
    position: Point<Pixels>,
    _subscriptions: [Subscription; 2],
}

impl EventEmitter<PluginManagerEvent> for PluginManager {}

impl Focusable for PluginManager {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl PluginManager {
    pub fn new(
        plugins: Entity<PluginStore>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search =
            cx.new(|cx| TextInput::new(tr("Search installed plugins"), cx).icon(IconName::Search));
        let subscriptions = vec![
            cx.subscribe(&search, |this, _, _: &InputEvent, cx| {
                this.keep_selection_listed(cx);
                cx.notify()
            }),
            cx.observe(&plugins, |this, _, cx| {
                this.plugins_changed(cx);
                cx.notify()
            }),
        ];
        let mut manager = Self {
            plugins,
            workspace,
            search,
            selected: None,
            tab: Tab::Overview,
            log_lines: Vec::new(),
            log_scroll: UniformListScrollHandle::new(),
            menu: None,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        manager.plugins_changed(cx);
        #[cfg(feature = "scenario")]
        manager.scenario_start(window, cx);
        #[cfg(not(feature = "scenario"))]
        let _ = window;
        manager
    }

    /// Shows a plugin on the right.
    pub fn select(&mut self, id: SharedString, cx: &mut Context<Self>) {
        if self.selected.as_ref() != Some(&id) {
            self.selected = Some(id);
            self.log_lines.clear();
            cx.notify();
        }
    }

    /// Shows a plugin even if the search would hide it (one just installed): the search is
    /// cleared.
    pub fn reveal(&mut self, id: SharedString, cx: &mut Context<Self>) {
        if !self.search.read(cx).is_empty() {
            self.search.update(cx, |search, cx| search.set_text("", cx));
        }
        self.select(id, cx);
    }

    /// The Plugins page was opened: typing goes to the search.
    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.search.focus_handle(cx));
    }

    /// The plugins of the store, in the order of the list (groups, then the store's order).
    fn shown(&self, cx: &App) -> Vec<Shown> {
        let store = self.plugins.read(cx);
        let mut shown: Vec<Shown> = store
            .plugins()
            .iter()
            .map(|plugin| Shown {
                id: plugin.id().to_string().into(),
                entry: plugin.entry.clone(),
                status: plugin.status.clone(),
                log: plugin.log.clone(),
            })
            .collect();
        shown.sort_by_key(|plugin| {
            GROUPS
                .iter()
                .position(|group| *group == plugin.entry.source)
                .unwrap_or(GROUPS.len())
        });
        shown
    }

    fn query(&self, cx: &App) -> String {
        self.search.read(cx).text()
    }

    /// The plugins the list shows: those the search finds.
    fn listed(&self, cx: &App) -> Vec<Shown> {
        let query = self.query(cx);
        self.shown(cx)
            .into_iter()
            .filter(|plugin| plugin.matches(&query))
            .collect()
    }

    /// The store changed: icons for the new plugins, and a selection that still exists.
    fn plugins_changed(&mut self, cx: &mut Context<Self>) {
        for plugin in self.shown(cx) {
            if plugin.manifest().icon.is_some() {
                icons::register_plugin_files(&plugin.id, plugin.entry.files.clone());
            }
        }
        self.keep_selection_listed(cx);
    }

    /// The selection follows the list: a plugin that went away or that the search hides gives
    /// way to the first one listed.
    fn keep_selection_listed(&mut self, cx: &mut Context<Self>) {
        let listed = self.listed(cx);
        let kept = self
            .selected
            .as_ref()
            .is_some_and(|id| listed.iter().any(|plugin| &plugin.id == id));
        if !kept {
            self.selected = listed.first().map(|plugin| plugin.id.clone());
            self.log_lines.clear();
        }
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(1, cx);
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(-1, cx);
    }

    fn move_selection(&mut self, step: isize, cx: &mut Context<Self>) {
        let listed = self.listed(cx);
        if listed.is_empty() {
            return;
        }
        let current = self
            .selected
            .as_ref()
            .and_then(|id| listed.iter().position(|plugin| &plugin.id == id));
        let next = match current {
            Some(index) => (index as isize + step).clamp(0, listed.len() as isize - 1) as usize,
            None => 0,
        };
        self.select(listed[next].id.clone(), cx);
    }

    fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.tab = tab;
            self.log_lines.clear();
            cx.notify();
        }
    }

    fn set_enabled(&mut self, id: &str, enabled: bool, cx: &mut Context<Self>) {
        self.plugins
            .update(cx, |store, cx| store.set_enabled(id, enabled, cx));
        cx.notify();
    }

    /// «Install Plugin from Disk…» of the gear menu (or the palette while the page has the
    /// focus): the installed plugin gets selected here.
    fn install(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        run_install(self.workspace.clone(), Some(cx.weak_entity()), window, cx);
    }

    /// «Reload Development Plugins»: every plugin under development is rebuilt and reloaded.
    fn reload_dev(&mut self, cx: &mut Context<Self>) {
        let dev: Vec<SharedString> = self
            .shown(cx)
            .into_iter()
            .filter(|plugin| plugin.entry.source == PluginSource::Dev && plugin.on())
            .map(|plugin| plugin.id)
            .collect();
        self.plugins.update(cx, |store, cx| {
            for id in &dev {
                store.reload(id, cx);
            }
        });
    }

    /// Asks, then removes an installed plugin or unlinks one under development.
    fn uninstall(&mut self, plugin: &Shown, window: &mut Window, cx: &mut Context<Self>) {
        let name = plugin.name();
        let message = match plugin.entry.source {
            PluginSource::Dev => {
                tr("Flux stops using the plugin's folder; the folder itself stays on disk.")
            }
            _ => tr("The plugin is deleted; its data and settings stay, in case it comes back."),
        };
        let answer = Dialog::warning(trf("Uninstall “{0}”?", &[&name]))
            .message(message)
            .danger(tr("Uninstall"))
            .cancel(tr("Cancel"))
            .show(window, cx);
        let id = plugin.id.clone();
        let plugins = self.plugins.clone();
        cx.spawn_in(window, async move |_, cx| {
            if answer.await != Some(0) {
                return;
            }
            let result = plugins.update(cx, |store, cx| store.uninstall(&id, cx));
            if let Ok(Err(reason)) = result {
                Dialog::warning(trf("Couldn't uninstall “{0}”", &[&name]))
                    .message(reason)
                    .primary(tr("OK"))
                    .show_async(cx)
                    .await;
            }
        })
        .detach();
    }

    /// What stopped the plugin, in full: the error and its details (the backtrace, the build
    /// output).
    fn show_stop_details(&mut self, plugin: &Shown, window: &mut Window, cx: &mut Context<Self>) {
        let PluginStatus::Stopped { error, details } = &plugin.status else {
            return;
        };
        let mut dialog = Dialog::warning(trf("“{0}” stopped", &[&plugin.name()]))
            .message(error.clone())
            .primary(tr("OK"));
        if !details.is_empty() {
            dialog = dialog.details(details.clone());
        }
        // The answer doesn't matter: the dialog only tells.
        drop(dialog.show(window, cx));
    }

    fn open_menu(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        // The items' actions are dispatched where the focus was: here, so the page handles them.
        window.focus(&self.focus_handle);
        let has_dev = self
            .shown(cx)
            .iter()
            .any(|plugin| plugin.entry.source == PluginSource::Dev);
        let menu = cx.new(|cx| {
            ContextMenu::new(window, cx)
                .entry(tr("Install Plugin from Disk…"), plugins::InstallFromDisk)
                .separator()
                .entry_if(
                    has_dev,
                    tr("Reload Development Plugins"),
                    plugins::ReloadDevPlugins,
                )
        });
        let focus = menu.focus_handle(cx);
        let subscriptions = [
            cx.subscribe_in(&menu, window, |this, menu, _: &DismissEvent, window, cx| {
                this.close_menu(menu, window, cx)
            }),
            cx.on_focus_out(&focus, window, {
                let menu = menu.clone();
                move |this, _, window, cx| this.close_menu(&menu, window, cx)
            }),
        ];
        window.focus(&focus);
        self.menu = Some(OpenMenu {
            menu,
            position,
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    fn close_menu(
        &mut self,
        menu: &Entity<ContextMenu>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.menu.as_ref().is_none_or(|open| open.menu != *menu) {
            return;
        }
        let had_focus = menu.focus_handle(cx).contains_focused(window, cx);
        self.menu = None;
        if had_focus {
            window.focus(&self.focus_handle);
        }
        cx.notify();
    }

    /// The scenario's start (the `scenario` feature): `FLUX_SCENARIO_PLUGIN=<id>` selects a
    /// plugin, `FLUX_SCENARIO_PLUGIN_TAB=permissions|contributions|log` a tab,
    /// `FLUX_SCENARIO_INSTALL_PATH` starts installing that path.
    #[cfg(feature = "scenario")]
    fn scenario_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Ok(id) = std::env::var("FLUX_SCENARIO_PLUGIN") {
            self.selected = Some(id.into());
        }
        if let Ok(tab) = std::env::var("FLUX_SCENARIO_PLUGIN_TAB") {
            self.tab = match tab.as_str() {
                "permissions" => Tab::Permissions,
                "contributions" => Tab::Contributions,
                "log" => Tab::Log,
                _ => Tab::Overview,
            };
            // A log to look at: sample lines of every level.
            if self.tab == Tab::Log
                && let Some(plugin) = self
                    .selected
                    .as_ref()
                    .and_then(|id| self.shown(cx).into_iter().find(|p| &p.id == id))
            {
                for index in 0..40 {
                    let level = [Level::Info, Level::Debug, Level::Warn, Level::Info][index % 4];
                    plugin
                        .log
                        .write(level, &format!("line {index}: scanned 12 files"));
                }
                plugin.log.write(
                    Level::Error,
                    "panicked at src/lib.rs:42: index out of bounds",
                );
            }
        }
        if std::env::var_os("FLUX_SCENARIO_INSTALL_PATH").is_some() {
            let manager = cx.weak_entity();
            let workspace = self.workspace.clone();
            window.defer(cx, move |window, cx| {
                run_install(workspace, Some(manager), window, cx)
            });
        }
    }
}

// --- Drawing ---

impl PluginManager {
    /// The page's title, a line about plugins, and the gear menu.
    fn render_header(&self, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let menu_open = self.menu.is_some();
        div()
            .flex_none()
            .px_4()
            .pt_4()
            .pb_3()
            .flex()
            .items_start()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(theme::TEXT_LG))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(tr("Plugins")),
                    )
                    .child(div().text_color(ui.text_muted).child(tr(
                        "Plugins add commands, tool windows and settings. Each runs in its own sandbox: a plugin can't crash Flux, and it reaches only what its permissions allow.",
                    ))),
            )
            .child(
                ui::toggle_button("plugins-gear", IconName::Settings, menu_open, ui)
                    .tooltip(ui::tooltip(tr("Install and Reload"), None))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.open_menu(event.position, window, cx)
                        }),
                    ),
            )
    }

    /// The list: the search field, then the groups of plugins.
    fn render_list(&self, listed: &[Shown], any: bool, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let mut rows: Vec<AnyElement> = Vec::new();
        for group in GROUPS {
            let members: Vec<&Shown> = listed
                .iter()
                .filter(|plugin| plugin.entry.source == group)
                .collect();
            if members.is_empty() {
                continue;
            }
            rows.push(
                div()
                    .px_2()
                    .pt_2()
                    .pb_1()
                    .child(ui::section_label(group_label(group), ui))
                    .into_any_element(),
            );
            for plugin in members {
                rows.push(self.render_row(plugin, cx).into_any_element());
            }
        }
        // Folders whose manifest can't be read: listed, so that a plugin under development with a
        // mistake in its manifest doesn't just vanish.
        let broken: Vec<ScanError> = self.plugins.read(cx).scan_errors().to_vec();
        if !broken.is_empty() && self.search.read(cx).is_empty() {
            rows.push(
                div()
                    .px_2()
                    .pt_2()
                    .pb_1()
                    .child(ui::section_label(tr("Couldn't Load"), ui))
                    .into_any_element(),
            );
            for (index, error) in broken.into_iter().enumerate() {
                rows.push(broken_row(index, error, ui).into_any_element());
            }
        }
        let body = if rows.is_empty() {
            let (title, hint) = if any {
                (
                    tr("No plugins found").to_string(),
                    tr("Search looks in names, descriptions and authors.").to_string(),
                )
            } else {
                (
                    tr("No plugins yet").to_string(),
                    tr("Install one from a folder or an archive: the gear menu above.").to_string(),
                )
            };
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .px_4()
                .text_center()
                .child(icon(IconName::Puzzle, ui.dim).size(px(24.)))
                .child(div().text_color(ui.text_muted).child(title))
                .child(
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.dim)
                        .child(hint),
                )
                .into_any_element()
        } else {
            div()
                .id("plugin-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px_1()
                .pb_1()
                .flex()
                .flex_col()
                .gap_0p5()
                .children(rows)
                .into_any_element()
        };
        div()
            .flex_none()
            .w(px(LIST_WIDTH))
            .h_full()
            .flex()
            .flex_col()
            .rounded(px(ui::RADIUS_MD))
            .border_1()
            .border_color(ui.island_border)
            .child(div().flex_none().p_2().child(self.search.clone()))
            .child(body)
    }

    /// A plugin in the list: its icon, name and version, what it's doing, and its switch.
    fn render_row(&self, plugin: &Shown, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let selected = self.selected.as_ref() == Some(&plugin.id);
        let on = plugin.on();
        let available = plugin.available();
        let (status, status_color) = status_line(plugin, ui);
        let id = plugin.id.clone();
        let switch_id = plugin.id.clone();
        div()
            .id(SharedString::from(format!("plugin-row-{}", plugin.id)))
            .flex()
            .items_center()
            .gap_2p5()
            .px_2()
            .py_1p5()
            .rounded(px(ui::RADIUS_SM))
            .cursor_pointer()
            .when(selected, |row| row.bg(ui.list_selected))
            .when(!selected, |row| row.hover(move |style| style.bg(ui.hover)))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.select(id.clone(), cx)))
            .child(plugin_icon(
                plugin,
                LIST_ICON,
                if on { ui.text_muted } else { ui.dim },
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap_1p5()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(if on { ui.foreground } else { ui.text_muted })
                                    .child(plugin.name()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(px(theme::TEXT_XS))
                                    .text_color(ui.dim)
                                    .child(plugin.manifest().version.clone()),
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
                ui::switch(
                    SharedString::from(format!("plugin-switch-{}", plugin.id)),
                    on,
                    ui,
                )
                .when(!available, |switch| switch.opacity(0.4).cursor_default())
                .when(available, |switch| {
                    switch.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        this.set_enabled(&switch_id, !on, cx)
                    }))
                }),
            )
    }

    /// The selected plugin: who it is, its buttons, the tabs.
    fn render_details(&mut self, plugin: &Shown, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let manifest = plugin.manifest().clone();
        // Where it comes from (a badge: a word for one plugin, unlike the list's group labels).
        let (source_label, source_color) = match plugin.entry.source {
            PluginSource::Bundled => (tr("Built-in"), ui.text_muted),
            PluginSource::Installed => (tr("Installed"), ui.accent_text),
            PluginSource::Dev => (tr("Development"), ui.amber),
        };
        let mut meta: Vec<AnyElement> = Vec::new();
        if !manifest.authors.is_empty() {
            meta.push(
                div()
                    .min_w_0()
                    .truncate()
                    .child(manifest.authors.join(", "))
                    .into_any_element(),
            );
        }
        if let Some(repository) = manifest.repository.clone() {
            let shown = repository
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .to_string();
            meta.push(
                div()
                    .id("plugin-repository")
                    .min_w_0()
                    .truncate()
                    .text_color(ui.accent_text)
                    .cursor_pointer()
                    .hover(|style| style.underline())
                    .on_click(move |_, _, cx| cx.open_url(&repository))
                    .child(shown)
                    .into_any_element(),
            );
        }
        let identity = div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .flex_none()
                    .size(px(DETAILS_ICON + 14.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(ui::RADIUS_MD))
                    .bg(ui.input_background)
                    .border_1()
                    .border_color(ui.island_border)
                    .child(plugin_icon(
                        plugin,
                        DETAILS_ICON - 8.,
                        if plugin.on() { ui.accent_text } else { ui.dim },
                    )),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
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
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(theme::TEXT_LG))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(plugin.name()),
                            )
                            .child(ui::badge(manifest.version.clone(), ui.text_muted))
                            .child(ui::badge(source_label, source_color)),
                    )
                    .when(!meta.is_empty(), |column| {
                        column.child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .text_size(px(theme::TEXT_SM))
                                .text_color(ui.text_muted)
                                .children(dot_separated(meta, ui)),
                        )
                    }),
            );
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .rounded(px(ui::RADIUS_MD))
            .border_1()
            .border_color(ui.island_border)
            .child(
                div()
                    .flex_none()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(identity)
                    .child(self.render_buttons(plugin, cx))
                    .children(self.render_status_banner(plugin, cx)),
            )
            .child(self.render_tabs(cx))
            .child(ui::divider(ui))
            .child(self.render_tab(plugin, cx))
    }

    /// The plugin's buttons: on / off, restart, reload, settings, uninstall.
    fn render_buttons(&self, plugin: &Shown, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let on = plugin.on();
        let mut buttons: Vec<AnyElement> = Vec::new();
        if plugin.available() {
            let id = plugin.id.clone();
            let button = if on {
                ui::text_button("plugin-disable", tr("Disable"), false, ui)
            } else {
                ui::primary_button("plugin-enable", tr("Enable"), true, ui)
            };
            buttons.push(
                button
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.set_enabled(&id, !on, cx)
                    }))
                    .into_any_element(),
            );
        }
        if matches!(plugin.status, PluginStatus::Stopped { .. }) {
            let id = plugin.id.clone();
            buttons.push(
                ui::text_button("plugin-restart", tr("Restart"), false, ui)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.plugins.update(cx, |store, cx| store.restart(&id, cx))
                    }))
                    .into_any_element(),
            );
        }
        if plugin.entry.source == PluginSource::Dev && on {
            let id = plugin.id.clone();
            buttons.push(
                ui::text_button("plugin-reload", tr("Reload"), false, ui)
                    .tooltip(ui::tooltip(
                        tr("Builds the plugin with cargo and loads it again"),
                        None,
                    ))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.plugins.update(cx, |store, cx| store.reload(&id, cx))
                    }))
                    .into_any_element(),
            );
        }
        if on && !plugin.manifest().settings.is_empty() {
            let id = plugin.id.clone();
            buttons.push(
                ui::text_button("plugin-settings", tr("Settings"), false, ui)
                    .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                        cx.emit(PluginManagerEvent::OpenSettings(id.clone()))
                    }))
                    .into_any_element(),
            );
        }
        if plugin.entry.source != PluginSource::Bundled {
            let shown = plugin.clone();
            buttons.push(div().flex_1().into_any_element());
            buttons.push(
                ui::text_button("plugin-uninstall", tr("Uninstall"), true, ui)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.uninstall(&shown, window, cx)
                    }))
                    .into_any_element(),
            );
        }
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1p5()
            .children(buttons)
    }

    /// Why the plugin isn't running, when it's not: stopped by itself, or unavailable.
    fn render_status_banner(&self, plugin: &Shown, cx: &mut Context<Self>) -> Option<Div> {
        let ui = Theme::ui(cx);
        let (color, icon_name, text, details) = match &plugin.status {
            PluginStatus::Stopped { error, details } => (
                ui.error,
                IconName::Error,
                trf("Stopped: {0}", &[error]),
                !details.is_empty(),
            ),
            PluginStatus::Unavailable(reason) => (
                ui.warning,
                IconName::Warning,
                unavailable_reason(&plugin.entry, reason),
                false,
            ),
            _ => return None,
        };
        let shown = plugin.clone();
        Some(
            div()
                .flex()
                .items_start()
                .gap_2()
                .px_2p5()
                .py_2()
                .rounded(px(ui::RADIUS_SM))
                .bg(UiColors::tint(color, 0.10))
                .border_1()
                .border_color(UiColors::tint(color, 0.30))
                .child(div().pt(px(2.)).child(icon(icon_name, color).size(px(13.))))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.foreground)
                        .child(text),
                )
                .when(details, |banner| {
                    banner.child(
                        div()
                            .id("plugin-stop-details")
                            .flex_none()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.accent_text)
                            .cursor_pointer()
                            .hover(|style| style.underline())
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.show_stop_details(&shown, window, cx)
                            }))
                            .child(tr("Details")),
                    )
                }),
        )
    }

    /// The tabs: Overview, Permissions, Contributions, Log.
    fn render_tabs(&self, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let tabs = Tab::ALL.into_iter().map(|tab| {
            let active = self.tab == tab;
            let group = SharedString::from(format!("plugin-tab-{tab:?}"));
            div()
                .id(group.clone())
                .group(group.clone())
                .relative()
                .h_full()
                .flex()
                .items_center()
                .cursor_pointer()
                .child(
                    ui::section_label(tab.label(), ui)
                        .when(active, |label| label.text_color(ui.foreground))
                        .when(!active, |label| {
                            label.group_hover(group.clone(), move |style| {
                                style.text_color(ui.text_muted)
                            })
                        }),
                )
                // The active tab is underlined with the accent.
                .when(active, |tab| {
                    tab.child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .bottom(px(6.))
                            .h(px(2.))
                            .rounded(px(1.))
                            .bg(ui.accent),
                    )
                })
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_tab(tab, cx)))
        });
        div()
            .flex_none()
            .h(px(TABS_HEIGHT))
            .px_3()
            .flex()
            .items_center()
            .gap_4()
            .children(tabs)
    }

    fn render_tab(&mut self, plugin: &Shown, cx: &mut Context<Self>) -> AnyElement {
        match self.tab {
            Tab::Log => self.render_log(plugin, cx).into_any_element(),
            tab => {
                let content = match tab {
                    Tab::Overview => self.render_overview(plugin, cx),
                    Tab::Permissions => self.render_permissions(plugin, cx),
                    _ => self.render_contributions(plugin, cx),
                };
                div()
                    .id("plugin-tab-content")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_3()
                    .child(content)
                    .into_any_element()
            }
        }
    }

    /// The description (Markdown), then the facts: the id, the API, the folder.
    fn render_overview(&self, plugin: &Shown, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let manifest = plugin.manifest();
        let description = translated(&plugin.entry, &manifest.description);
        let text = if description.trim().is_empty() {
            div()
                .text_color(ui.dim)
                .child(tr("The plugin has no description."))
                .into_any_element()
        } else {
            crate::markdown::render(
                &crate::markdown::parse(description),
                ui.foreground,
                Theme::get(cx),
            )
        };
        let mut facts: Vec<(&'static str, AnyElement)> = vec![
            (tr("Identifier"), code_text(manifest.id.clone(), ui)),
            (tr("Plugin API"), code_text(manifest.api.clone(), ui)),
        ];
        if let Some(dir) = plugin.entry.files.dir() {
            let path = dir.to_path_buf();
            facts.push((
                tr("Folder"),
                div()
                    .id("plugin-folder")
                    .min_w_0()
                    .truncate()
                    .font_family(theme::code_font())
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.accent_text)
                    .cursor_pointer()
                    .hover(|style| style.underline())
                    .tooltip(ui::tooltip(tr("Show in Finder"), None))
                    .on_click(move |_, _, cx| cx.reveal_path(&path))
                    .child(tilde(dir))
                    .into_any_element(),
            ));
        }
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(text)
            .child(ui::divider(ui))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(facts.into_iter().map(|(label, value)| {
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(110.))
                                    .text_color(ui.dim)
                                    .child(label),
                            )
                            .child(div().flex_1().min_w_0().child(value))
                    })),
            )
    }

    /// What the plugin may do beyond the API, in plain words, and what the sandbox keeps from it.
    fn render_permissions(&self, plugin: &Shown, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let lines = permission_lines(&plugin.manifest().permissions);
        let list = if lines.is_empty() {
            vec![permission_row(
                IconName::CheckCircle,
                ui.success,
                tr("Asks for no special permissions"),
                ui,
            )]
        } else {
            lines
                .into_iter()
                .map(|(name, line)| permission_row(name, ui.accent_text, line, ui))
                .collect()
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().flex().flex_col().gap_1().children(list))
            .child(
                div()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.dim)
                    .child(tr(
                        "Every plugin may show notifications and questions, and add what its manifest declares. Beyond that, the sandbox lets it reach only its own folder and what is listed here: no other files, no network, no programs.",
                    )),
            )
    }

    /// What the plugin adds to the window: commands, tool windows, status bar items, settings.
    fn render_contributions(&self, plugin: &Shown, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let manifest = plugin.manifest();
        let entry = &plugin.entry;
        let mut sections: Vec<AnyElement> = Vec::new();
        if !manifest.commands.is_empty() {
            let category = |category: &Option<String>| {
                translated(entry, category.as_deref().unwrap_or(&manifest.name)).to_string()
            };
            let rows = manifest.commands.iter().map(|command| {
                contribution_row(
                    IconName::Command,
                    format!(
                        "{}: {}",
                        category(&command.category),
                        translated(entry, &command.title)
                    ),
                    command.keys.as_deref(),
                    ui,
                )
            });
            sections.push(contribution_section(tr("Commands"), rows, ui));
        }
        if !manifest.tool_windows.is_empty() {
            let rows = manifest.tool_windows.iter().map(|window| {
                contribution_row(
                    IconName::Sidebar,
                    translated(entry, &window.title).to_string(),
                    window.keys.as_deref(),
                    ui,
                )
            });
            sections.push(contribution_section(tr("Tool Windows"), rows, ui));
        }
        if !manifest.status_items.is_empty() {
            let count = manifest.status_items.len();
            sections.push(contribution_section(
                tr("Status Bar"),
                std::iter::once(contribution_row(
                    IconName::Info,
                    trn(count, "{n} item", "{n} items"),
                    None,
                    ui,
                )),
                ui,
            ));
        }
        if !manifest.settings.is_empty() {
            let count = manifest.settings.len();
            let id = plugin.id.clone();
            let open = plugin.on().then(|| {
                div()
                    .id("plugin-open-settings")
                    .flex_none()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.accent_text)
                    .cursor_pointer()
                    .hover(|style| style.underline())
                    .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                        cx.emit(PluginManagerEvent::OpenSettings(id.clone()))
                    }))
                    .child(tr("Open"))
            });
            sections.push(contribution_section(
                tr("Settings"),
                std::iter::once(
                    contribution_row(
                        IconName::Settings,
                        trn(count, "{n} setting", "{n} settings"),
                        None,
                        ui,
                    )
                    .children(open),
                ),
                ui,
            ));
        }
        if sections.is_empty() {
            return div()
                .text_color(ui.dim)
                .child(tr("The plugin adds nothing to the window."));
        }
        div().flex().flex_col().gap_4().children(sections)
    }

    /// The plugin's log: the newest line at the bottom, followed as lines come.
    fn render_log(&mut self, plugin: &Shown, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let lines = plugin.log.lines();
        if lines.len() != self.log_lines.len() && !lines.is_empty() {
            self.log_scroll
                .scroll_to_item(lines.len() - 1, ScrollStrategy::Bottom);
        }
        self.log_lines = lines;
        let count = self.log_lines.len();
        let path = plugin.log.path();
        let log = plugin.log.clone();
        let toolbar = div()
            .flex_none()
            .px_3()
            .py_2()
            .flex()
            .items_center()
            .gap_1p5()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.dim)
                    .child(trn(count, "{n} line", "{n} lines")),
            )
            .child(
                ui::text_button("plugin-log-clear", tr("Clear"), false, ui).on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| {
                        log.clear();
                        this.log_lines.clear();
                        cx.notify()
                    },
                )),
            )
            .children(path.filter(|path| path.exists()).map(|path| {
                ui::text_button("plugin-log-reveal", tr("Show in Finder"), false, ui)
                    .on_click(move |_, _, cx| cx.reveal_path(&path))
            }));
        let body = if count == 0 {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(ui.dim)
                .child(tr("Nothing in the log yet"))
                .into_any_element()
        } else {
            uniform_list(
                "plugin-log",
                count,
                cx.processor(|this, range: Range<usize>, _, cx| {
                    let ui = Theme::ui(cx);
                    range
                        .filter_map(|index| this.log_lines.get(index))
                        .map(|line| log_line(line, ui))
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(self.log_scroll.clone())
            .flex_1()
            .px_3()
            .pb_2()
            .into_any_element()
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(body)
    }
}

impl Render for PluginManager {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let any = !self.plugins.read(cx).plugins().is_empty();
        let listed = self.listed(cx);
        let selected = self
            .selected
            .as_ref()
            .and_then(|id| listed.iter().find(|plugin| &plugin.id == id))
            .cloned();
        let details = match selected {
            Some(plugin) => self.render_details(&plugin, cx).into_any_element(),
            None => {
                let ui = Theme::ui(cx);
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(ui::RADIUS_MD))
                    .border_1()
                    .border_color(ui.island_border)
                    .text_color(ui.dim)
                    .child(tr("Select a plugin to see its details"))
                    .into_any_element()
            }
        };
        div()
            .key_context("PluginManager")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(
                cx.listener(|this, _: &plugins::InstallFromDisk, window, cx| {
                    this.install(window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &plugins::ReloadDevPlugins, _, cx| this.reload_dev(cx)),
            )
            .size_full()
            .flex()
            .flex_col()
            .child(self.render_header(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px_4()
                    .pb_4()
                    .flex()
                    .gap_3()
                    .child(self.render_list(&listed, any, cx))
                    .child(details),
            )
            .children(
                self.menu
                    .as_ref()
                    .map(|menu| ContextMenu::overlay(&menu.menu, menu.position)),
            )
    }
}

/// What a plugin is doing, as its row says it, and the color.
fn status_line(plugin: &Shown, ui: UiColors) -> (SharedString, Hsla) {
    match &plugin.status {
        PluginStatus::Running => (tr("Running").into(), ui.dim),
        PluginStatus::Starting => (tr("Starting…").into(), ui.accent_text),
        PluginStatus::Stopped { error, .. } => (trf("Stopped: {0}", &[error]).into(), ui.error),
        PluginStatus::Unavailable(reason) => {
            (unavailable_reason(&plugin.entry, reason).into(), ui.warning)
        }
        PluginStatus::Disabled => (tr("Disabled").into(), ui.dim),
    }
}

/// Why a plugin can't run, in the interface language: the registry's reasons are English.
fn unavailable_reason(entry: &PluginEntry, reason: &str) -> String {
    let manifest = &entry.manifest;
    if manifest.api != flux_plugin::API_VERSION {
        return trf(
            "Built for plugin API {0}; this Flux has {1}",
            &[&manifest.api, &flux_plugin::API_VERSION],
        );
    }
    match &manifest.wasm {
        Some(wasm) if reason == format!("The component {wasm} is missing") => {
            trf("The component {0} is missing", &[wasm])
        }
        _ => reason.to_string(),
    }
}

/// The plugin's icon from its manifest (a monochrome SVG of its folder), or a puzzle piece.
fn plugin_icon(plugin: &Shown, size: f32, color: Hsla) -> AnyElement {
    match &plugin.manifest().icon {
        Some(file) => svg()
            .path(icons::plugin_asset(&plugin.id, file))
            .flex_none()
            .size(px(size))
            .text_color(color)
            .into_any_element(),
        None => icon(IconName::Puzzle, color)
            .flex_none()
            .size(px(size))
            .into_any_element(),
    }
}

/// Elements with a dot between them: "Egor Ageev · github.com/…".
fn dot_separated(items: Vec<AnyElement>, ui: UiColors) -> Vec<AnyElement> {
    let mut joined = Vec::new();
    for (index, item) in items.into_iter().enumerate() {
        if index > 0 {
            joined.push(
                div()
                    .flex_none()
                    .text_color(ui.dim)
                    .child("·")
                    .into_any_element(),
            );
        }
        joined.push(item);
    }
    joined
}

fn code_text(text: impl Into<SharedString>, ui: UiColors) -> AnyElement {
    div()
        .font_family(theme::code_font())
        .text_size(px(theme::TEXT_SM))
        .text_color(ui.foreground)
        .child(text.into())
        .into_any_element()
}

fn permission_row(name: IconName, color: Hsla, text: &'static str, ui: UiColors) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_2p5()
        .px_2()
        .py_1p5()
        .rounded(px(ui::RADIUS_SM))
        .bg(ui.input_background)
        .child(icon(name, color).size(px(14.)))
        .child(div().text_color(ui.foreground).child(text))
        .into_any_element()
}

/// A group of the Contributions tab: a label over its rows.
fn contribution_section(
    label: &'static str,
    rows: impl IntoIterator<Item = Div>,
    ui: UiColors,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().pb_0p5().child(ui::section_label(label, ui)))
        .children(rows)
        .into_any_element()
}

/// A row of the Contributions tab: what it is, and its default shortcut.
fn contribution_row(
    name: IconName,
    text: impl Into<SharedString>,
    keys: Option<&str>,
    ui: UiColors,
) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2p5()
        .px_2()
        .py_1()
        .rounded(px(ui::RADIUS_SM))
        .child(icon(name, ui.dim).size(px(13.)))
        .child(div().flex_1().min_w_0().truncate().child(text.into()))
        .children(keys.and_then(keys_label).map(|label| ui::keys(&label, ui)))
}

/// A shortcut of a manifest in gpui's notation ("alt-cmd-shift-t", "cmd-k cmd-s") as the
/// palette shows it ("⌥⇧⌘T", "⌘K ⌘S"); none when it doesn't parse.
fn keys_label(keys: &str) -> Option<String> {
    keys.split_whitespace()
        .map(|part| {
            Keystroke::parse(part)
                .ok()
                .map(|keystroke| keystroke_label(&keystroke.modifiers, &keystroke.key))
        })
        .collect::<Option<Vec<_>>>()
        .filter(|parts| !parts.is_empty())
        .map(|parts| parts.join(" "))
}

/// A line of the log: the time, the level, the text.
fn log_line(line: &LogLine, ui: UiColors) -> AnyElement {
    let (level, color) = match line.level {
        Level::Debug => ("DEBUG", ui.dim),
        Level::Info => ("INFO", ui.text_muted),
        Level::Warn => ("WARN", ui.warning),
        Level::Error => ("ERROR", ui.error),
    };
    div()
        .h(px(LOG_LINE_HEIGHT))
        .flex()
        .items_center()
        .gap_2()
        .font_family(theme::code_font())
        .text_size(px(theme::TEXT_XS + 1.))
        .whitespace_nowrap()
        .child(div().flex_none().text_color(ui.dim).child(clock(line.time)))
        .child(div().flex_none().w(px(40.)).text_color(color).child(level))
        .child(
            div()
                .min_w_0()
                .text_color(if line.level == Level::Error {
                    ui.error
                } else {
                    ui.foreground
                })
                .child(line.text.clone()),
        )
        .into_any_element()
}

/// "14:03:27" in the local time zone.
fn clock(time: SystemTime) -> String {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64);
    let local = (seconds + crate::git_log::local_offset(seconds)).rem_euclid(86_400);
    format!(
        "{:02}:{:02}:{:02}",
        local / 3600,
        local % 3600 / 60,
        local % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_plugin::registry::PluginFiles;

    fn candidate(manifest: &str, kind: CandidateKind) -> Candidate {
        let manifest = Manifest::parse(manifest).unwrap();
        Candidate {
            entry: PluginEntry {
                manifest,
                files: PluginFiles::Embedded(&[]),
                source: PluginSource::Installed,
                locales: Default::default(),
                problem: None,
            },
            path: PathBuf::from("/tmp/hello"),
            kind,
        }
    }

    const HELLO: &str = r#"
id = "someone.hello"
name = "Hello"
version = "1.2.0"
api = "0.1"
authors = ["Someone", "Another"]
"#;

    #[test]
    fn the_question_lists_who_and_what() {
        let plain = install_question(&candidate(HELLO, CandidateKind::Archive));
        assert_eq!(plain.title.as_ref(), "Install “Hello” 1.2.0?");
        let message = plain.message.unwrap();
        assert!(message.contains("By Someone, Another"));
        assert!(message.contains("no special permissions"));
        assert!(!message.contains("under development"));

        let reads = format!("{HELLO}\n[permissions]\nproject = \"read\"\n");
        let dev = install_question(&candidate(&reads, CandidateKind::Folder));
        let message = dev.message.unwrap();
        assert!(
            message.contains("will be able to:\n\n•  Read and search the files of the project")
        );
        assert!(message.contains("under development"));
        assert_eq!(dev.buttons[INSTALL].label.as_ref(), "Install");
    }

    #[test]
    fn shortcuts_read_as_the_palette_shows_them() {
        assert_eq!(keys_label("alt-cmd-shift-t").as_deref(), Some("⌥⇧⌘T"));
        assert_eq!(keys_label("cmd-k cmd-s").as_deref(), Some("⌘K ⌘S"));
        assert_eq!(keys_label(""), None);
    }

    #[test]
    fn search_finds_names_ids_and_authors() {
        let entry = candidate(HELLO, CandidateKind::Archive).entry;
        let shown = Shown {
            id: "someone.hello".into(),
            entry: Arc::new(entry),
            status: PluginStatus::Disabled,
            log: PluginLog::memory(),
        };
        assert!(shown.matches(""));
        assert!(shown.matches("hel"));
        assert!(shown.matches("SOMEONE.H"));
        assert!(shown.matches("another"));
        assert!(!shown.matches("todo"));
        assert!(!shown.on());
    }
}
