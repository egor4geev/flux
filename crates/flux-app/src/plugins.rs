//! The window's plugins (stage 8, ADR-029): the hub between `flux-plugin`'s instances and the
//! window. It knows the plugins (bundled, installed, under development) and which of them are on,
//! starts and stops their instances, carries their calls out (notifications, questions, documents,
//! tool windows, status bar items) and tells them what happens in the window (events).
//!
//! What a plugin adds comes from its manifest: commands go to the palette ([`RunCommand`] with the
//! plugin's keys), tool windows to the island on the right with an icon in the launchpad
//! ([`ToolKey`], [`crate::plugin_view::PluginView`]), status bar items to the status bar, settings
//! to Settings ([`crate::plugin_settings`]); Settings → Plugins ([`crate::plugin_manager`]) turns
//! plugins on and off, installs and removes them.
//!
//! An instance runs on its own thread; its calls come back as messages to [`PluginStore`] and are
//! carried out on the window's thread: those about the store's own state (views, status items,
//! notifications) by the store, those that need the window (questions, documents, and the 0.2
//! interfaces of [`crate::plugin_calls`]) by [`handle_call`].
//!
//! Part 8.2: a command gets its context (where it was run from, the active document and its
//! selections, the files of a menu — [`CommandOrigin`]); the problems a plugin publishes (a linter)
//! are kept here by plugin and file and put into the file's document under the plugin's own owner
//! ([`crate::diagnostics::plugin_owner`]) — now, when the file is opened later, when the plugin
//! stops (they go); `git-changed` and `diagnostics-changed` come from the window's Git and language
//! servers ([`PluginStore::set_window_parts`]). A plugin that stops by itself keeps its tool windows (they say it stopped and
//! offer a restart) and gets a notification with Restart, Disable and Details; a plugin under
//! development is built with cargo before it starts and reloaded when its component changes.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::{Duration, SystemTime};

use flux_core::Rope;
use flux_core::text::{line_len, line_start};
use flux_plugin::api::diagnostics::Diagnostic as ApiDiagnostic;
use flux_plugin::api::dialogs::{ButtonRole as ApiButtonRole, DialogLevel, Question, TextQuestion};
use flux_plugin::api::editors::TextEdit;
use flux_plugin::api::events::{Event, UiEvent};
use flux_plugin::api::notifications::{
    Notification as ApiNotification, NotificationKind as ApiKind, Progress as ApiProgress,
};
use flux_plugin::api::types::{
    CommandContext, CommandSource, EditorInfo, Position, Range as ApiRange,
};
use flux_plugin::api::ui::{ButtonSpec, Element, ElementKind, Span, Tone, View};
use flux_plugin::install::{Candidate, CandidateKind};
use flux_plugin::log::{Level, PluginLog};
use flux_plugin::manifest::NotificationDisplay;
use flux_plugin::registry::{PluginEntry, PluginSource, ScanError};
use flux_plugin::runtime::{
    EditorCall, EditorReply, HostCall, Instance, InstanceConfig, MessageKind, PluginMessage,
};
use futures::StreamExt;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use futures::channel::oneshot;
use gpui::{
    Action, AnyElement, App, AppContext as _, Context, Div, Entity, EntityId, EventEmitter, Global,
    KeyBinding, KeybindingKeystroke, Keystroke, PromptLevel, SharedString, Subscription, Task,
    WeakEntity, Window, actions, div, prelude::*, px,
};
use serde_json::{Map, Value};

use crate::dialog::{ButtonRole, Dialog};
use crate::editor::{Editor, EditorEvent};
use crate::i18n::{tr, trf};
use crate::input_dialog::InputDialog;
use crate::notification_center::{
    Display, GroupInfo, NotificationCenter, NotificationGroup, NotificationId,
};
use crate::notifications::{Notification, NotificationKind, Progress};
use crate::plugin_view::{PluginView, PluginViewEvent};
use crate::theme::Theme;
use crate::ui::{self, RADIUS_SM};
use crate::workspace::Workspace;
use crate::{icons, settings};

/// Runs a plugin's command: the palette, its keys, the context menus, notification actions,
/// status bar items.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = plugins, no_json)]
pub struct RunCommand {
    pub plugin: SharedString,
    pub command: SharedString,
    /// Where it was run from, and the files a menu was opened on; none — the command's keys.
    pub origin: Option<CommandOrigin>,
}

/// Where a plugin's command was run from (part 8.2): the command's context
/// ([`flux_plugin::api::types::CommandContext`]) is made from it when the command runs — the
/// active document and its selections, and these paths.
#[derive(Clone, PartialEq, Debug)]
pub struct CommandOrigin {
    pub source: CommandSource,
    /// The files and folders a menu was opened on: the rows selected in the tree, a tab's file.
    /// Empty — the active document's file.
    pub paths: Vec<PathBuf>,
}

impl CommandOrigin {
    pub fn new(source: CommandSource) -> Self {
        Self {
            source,
            paths: Vec::new(),
        }
    }

    pub fn with_paths(source: CommandSource, paths: Vec<PathBuf>) -> Self {
        Self { source, paths }
    }
}

/// Opens or hides a plugin's tool window: its launchpad icon, its keys.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = plugins, no_json)]
pub struct ToggleToolWindow {
    pub plugin: SharedString,
    pub window: SharedString,
}

/// Starts a stopped plugin again: the Restart of its notification.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = plugins, no_json)]
pub struct RestartPlugin(pub SharedString);

/// Turns a plugin off: the Disable of the notification about its stop.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = plugins, no_json)]
pub struct DisablePlugin(pub SharedString);

/// Why a plugin stopped, in a dialog with the whole story: the Details of its notification.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = plugins, no_json)]
pub struct ShowStopDetails(pub SharedString);

actions!(
    plugins,
    [
        /// Settings → Plugins.
        OpenManager,
        /// Install Plugin from Disk…: a folder (under development) or an archive.
        InstallFromDisk,
        /// Rebuilds and reloads the plugins under development.
        ReloadDevPlugins,
    ]
);

pub fn init(cx: &mut App) {
    let _ = cx;
}

/// The id of the element of a tool window's placeholder that restarts a stopped plugin.
const RESTART_ELEMENT: &str = "flux.restart";
/// How often the components of the plugins under development are looked at.
const DEV_WATCH: Duration = Duration::from_secs(1);
/// The files whose problems changed are told to the plugins together, this long after the first:
/// a check publishes many files in a burst.
const DIAGNOSTICS_DEBOUNCE: Duration = Duration::from_millis(150);
/// The most problems a plugin publishes for one file; the rest are dropped (with a line in its
/// log).
const MAX_PUBLISHED: usize = 5000;

/// A plugin tool window of the window: the key the right island and the launchpad know it by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ToolKey(pub u32);

/// What a plugin is doing.
#[derive(Debug, Clone, PartialEq)]
pub enum PluginStatus {
    /// Turned off by the user.
    Disabled,
    /// Can't run: an API version Flux doesn't have, a missing component.
    Unavailable(SharedString),
    /// Loading, or a plugin under development being built.
    Starting,
    Running,
    /// Stopped by itself: a trap, a limit, a failed start or build.
    Stopped {
        error: SharedString,
        details: SharedString,
    },
}

/// A plugin known to the window.
pub struct PluginState {
    pub entry: Arc<PluginEntry>,
    pub status: PluginStatus,
    pub log: PluginLog,
}

impl PluginState {
    pub fn id(&self) -> &str {
        self.entry.id()
    }

    pub fn source(&self) -> PluginSource {
        self.entry.source
    }

    /// The manifest's string in the interface language.
    pub fn tr<'a>(&'a self, text: &'a str) -> &'a str {
        self.entry.translate(crate::i18n::lang_code(), text)
    }

    /// The plugin's name in the interface language.
    pub fn name(&self) -> &str {
        self.tr(&self.entry.manifest.name)
    }
}

/// A plugin's tool window.
pub struct ToolWindow {
    pub key: ToolKey,
    pub plugin: SharedString,
    pub id: SharedString,
    /// In the interface language.
    pub title: SharedString,
    /// The icon's asset path ([`crate::icons::plugin_asset`]); none — the default icon.
    pub icon: Option<SharedString>,
    pub view: Entity<PluginView>,
}

/// A plugin's status bar item, as the plugin last set it.
pub struct StatusItem {
    pub plugin: SharedString,
    pub id: SharedString,
    pub text: SharedString,
    pub tooltip: Option<SharedString>,
    pub command: Option<RunCommand>,
}

/// A plugin's command for the palette.
pub struct PaletteCommand {
    pub action: RunCommand,
    /// The chip: the manifest's category or the plugin's name, translated.
    pub category: String,
    /// Translated.
    pub title: String,
    /// The plugin grayed it out (`commands.set-enabled`).
    pub enabled: bool,
}

/// A plugin's tool window for the palette: «TODO: TODO Window» opens it as its icon does.
pub struct PaletteToolWindow {
    pub action: ToggleToolWindow,
    /// The plugin's name, translated.
    pub category: String,
    /// The window's title, translated.
    pub title: String,
}

pub enum PluginStoreEvent {
    /// Plugins were turned on or off, installed or removed, their tool windows changed: the
    /// launchpad, the palette and Settings follow.
    Changed,
    /// A plugin's call that needs the window: a question, a document.
    Call {
        plugin: Arc<str>,
        call: HostCall,
    },
    /// A plugin asks to show or to hide its tool window.
    ShowToolWindow(ToolKey),
    HideToolWindow(ToolKey),
    /// A plugin stopped — turned off, reloaded, removed, or by itself: the window forgets its
    /// terminals (their tabs stay, the user's) and closes its proposals (part 8.2).
    PluginStopped(Arc<str>),
}

/// A plugin's question with a text field, waiting for the overlay window to free up.
struct PendingQuestion {
    plugin: Arc<str>,
    id: u64,
    question: TextQuestion,
}

/// The plugins of a window.
pub struct PluginStore {
    root: Option<PathBuf>,
    /// The width of the island on the right, shared by the tool windows.
    width: ui::RightIslandWidth,
    plugins: Vec<PluginState>,
    /// Folders that look like plugins but can't be read (a broken manifest).
    scan_errors: Vec<ScanError>,
    /// The running instances, by plugin id.
    instances: HashMap<Arc<str>, Instance>,
    /// Bumped whenever a plugin is started or stopped: a build or an answer that comes back for
    /// an earlier run is dropped.
    generations: HashMap<Arc<str>, u64>,
    tool_windows: Vec<ToolWindow>,
    next_tool: u32,
    /// The tool windows that show the window's own placeholder (stopped, building) rather than
    /// the plugin's view.
    placeholders: HashSet<ToolKey>,
    /// The tool windows on screen: a plugin that starts (again) hears that they are shown.
    visible_tools: HashSet<ToolKey>,
    _view_subscriptions: HashMap<ToolKey, Subscription>,
    status_items: Vec<StatusItem>,
    /// Commands the plugins grayed out: (plugin, command).
    disabled_commands: HashSet<(Arc<str>, String)>,
    messages: UnboundedSender<PluginMessage>,
    /// The window's notification center: the plugins' notifications and the store's own.
    center: Option<WeakEntity<NotificationCenter>>,
    /// The plugins' notifications: (plugin, the plugin's id) → the window's.
    notification_ids: HashMap<(Arc<str>, u64), NotificationId>,
    /// The notification about a plugin's stop, expired when it starts again.
    stop_notifications: HashMap<Arc<str>, NotificationId>,
    /// The documents the plugins hear about.
    editors: HashMap<EntityId, [Subscription; 2]>,
    /// The same documents by the API's id: a command's context reads the active one.
    editor_handles: HashMap<u64, WeakEntity<Editor>>,
    /// The problems the plugins published (part 8.2): by plugin and file (absolute, as published).
    published: HashMap<Arc<str>, HashMap<PathBuf, Vec<ApiDiagnostic>>>,
    /// Files whose problems changed since the plugins last heard ([`DIAGNOSTICS_DEBOUNCE`]).
    changed_diagnostics: BTreeSet<PathBuf>,
    diagnostics_flush: Option<Task<()>>,
    /// The state of the repositories the plugins last heard of: `git-changed` comes when it
    /// changes.
    git_state: Option<u64>,
    /// Subscriptions to the window's Git and language servers.
    _window_parts: Vec<Subscription>,
    /// The document the plugins were last told is active.
    active_editor: Option<u64>,
    /// The component of each plugin under development as it was when it started: a newer one is
    /// reloaded.
    dev_components: HashMap<Arc<str>, Option<SystemTime>>,
    /// The declarative files of each plugin under development (its manifest, queries, grammars,
    /// themes, icon sets) as they were when it started: a change reloads it (stage 8.3).
    dev_files: HashMap<Arc<str>, Vec<(String, Option<SystemTime>)>>,
    /// Plugins under development being built.
    building: HashSet<Arc<str>>,
    pending_questions: VecDeque<PendingQuestion>,
    _pump: Task<()>,
    _dev_watch: Task<()>,
}

impl EventEmitter<PluginStoreEvent> for PluginStore {}

impl PluginStore {
    /// Finds the plugins and starts the ones that are on. `width` is the right island's, shared
    /// with the plugins' tool windows.
    pub fn new(root: Option<PathBuf>, width: ui::RightIslandWidth, cx: &mut Context<Self>) -> Self {
        let (messages, mut receiver) = unbounded::<PluginMessage>();
        let pump = cx.spawn(async move |this, cx| {
            while let Some(message) = receiver.next().await {
                if this
                    .update(cx, |this, cx| this.handle_message(message, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        let dev_watch = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(DEV_WATCH).await;
                if this
                    .update(cx, |this, cx| this.check_dev_components(cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        let store = Self {
            root,
            width,
            plugins: Vec::new(),
            scan_errors: Vec::new(),
            instances: HashMap::new(),
            generations: HashMap::new(),
            tool_windows: Vec::new(),
            next_tool: 0,
            placeholders: HashSet::new(),
            visible_tools: HashSet::new(),
            _view_subscriptions: HashMap::new(),
            status_items: Vec::new(),
            disabled_commands: HashSet::new(),
            messages,
            center: None,
            notification_ids: HashMap::new(),
            stop_notifications: HashMap::new(),
            editors: HashMap::new(),
            editor_handles: HashMap::new(),
            published: HashMap::new(),
            changed_diagnostics: BTreeSet::new(),
            diagnostics_flush: None,
            git_state: None,
            _window_parts: Vec::new(),
            active_editor: None,
            dev_components: HashMap::new(),
            dev_files: HashMap::new(),
            building: HashSet::new(),
            pending_questions: VecDeque::new(),
            _pump: pump,
            _dev_watch: dev_watch,
        };
        // The plugins start once the window has made the store and given it its notification
        // center: a build of a plugin under development shows its progress there.
        let this = cx.weak_entity();
        cx.defer(move |cx| {
            this.update(cx, |store, cx| store.rescan(cx)).ok();
        });
        store
    }

    /// Where the store's notifications go (the plugins' and its own): the window's center. Until
    /// it is set, notifications are only logged.
    pub(crate) fn set_notification_center(&mut self, center: WeakEntity<NotificationCenter>) {
        self.center = Some(center);
    }

    /// All the plugins: bundled first, then by name.
    pub fn plugins(&self) -> &[PluginState] {
        &self.plugins
    }

    pub fn plugin(&self, id: &str) -> Option<&PluginState> {
        self.plugins.iter().find(|plugin| plugin.id() == id)
    }

    /// Folders that look like plugins but can't be read: a broken manifest.
    pub fn scan_errors(&self) -> &[ScanError] {
        &self.scan_errors
    }

    /// Turns a plugin on (starts it) or off (stops it), and remembers the choice; no restart.
    pub fn set_enabled(&mut self, id: &str, enabled: bool, cx: &mut Context<Self>) {
        settings::set_plugin_enabled(id, enabled, cx);
        let Some(index) = self.index(id) else {
            return;
        };
        if enabled {
            if self.plugins[index].entry.problem.is_none() {
                self.start(id, true, cx);
            }
        } else {
            self.stop(id, cx);
            self.remove_contributions(id, cx);
            self.plugins[index].status = PluginStatus::Disabled;
            self.plugins[index]
                .log
                .write(Level::Info, "Turned off by the user");
        }
        self.changed(cx);
    }

    /// Starts a stopped plugin again (the Restart of its notification, the manager); a plugin
    /// under development is built and read from its folder anew.
    pub fn restart(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(index) = self.index(id) else {
            return;
        };
        if self.plugins[index].entry.problem.is_some() || !settings::plugin_enabled(id, cx) {
            return;
        }
        if self.plugins[index].source() == PluginSource::Dev {
            return self.reload_from_disk(id, true, cx);
        }
        self.start(id, false, cx);
        self.changed(cx);
    }

    /// Rebuilds a plugin under development (cargo) and reloads it; any other plugin restarts.
    pub fn reload(&mut self, id: &str, cx: &mut Context<Self>) {
        match self.plugin(id).map(PluginState::source) {
            Some(PluginSource::Dev) => self.reload_from_disk(id, true, cx),
            Some(_) => self.restart(id, cx),
            None => {}
        }
    }

    /// Rebuilds and reloads every plugin under development.
    pub fn reload_dev_plugins(&mut self, cx: &mut Context<Self>) {
        let dev: Vec<String> = self
            .plugins
            .iter()
            .filter(|plugin| plugin.source() == PluginSource::Dev)
            .map(|plugin| plugin.id().to_string())
            .collect();
        for id in dev {
            self.reload_from_disk(&id, true, cx);
        }
    }

    /// Installs a plugin the user agreed to (after the question about its permissions) and
    /// starts it.
    pub fn install(&mut self, candidate: Candidate, cx: &mut Context<Self>) -> Result<(), String> {
        let folder = (candidate.kind == CandidateKind::Folder).then(|| candidate.path.clone());
        let entry = flux_plugin::install::install(candidate)?;
        let id = entry.id().to_string();
        if let Some(folder) = folder {
            settings::add_dev_plugin(folder, cx);
        }
        settings::set_plugin_enabled(&id, true, cx);
        // An older version of the plugin may be running: it gives way to the new one.
        self.stop(&id, cx);
        self.rescan(cx);
        if self
            .plugin(&id)
            .is_some_and(|plugin| plugin.entry.problem.is_none())
        {
            self.start(&id, true, cx);
        }
        self.changed(cx);
        Ok(())
    }

    /// Stops and removes an installed plugin, or unlinks one under development.
    pub fn uninstall(&mut self, id: &str, cx: &mut Context<Self>) -> Result<(), String> {
        let Some(index) = self.index(id) else {
            return Err(tr("The plugin is not installed").into());
        };
        let entry = self.plugins[index].entry.clone();
        match entry.source {
            PluginSource::Bundled => {
                return Err(tr("A bundled plugin can't be removed; turn it off instead").into());
            }
            PluginSource::Installed => flux_plugin::install::uninstall(&entry)?,
            PluginSource::Dev => {
                if let Some(dir) = entry.files.dir() {
                    settings::remove_dev_plugin(dir, cx);
                }
            }
        }
        self.stop(id, cx);
        self.remove_contributions(id, cx);
        // A bundled or installed plugin with the same id comes back from under it.
        self.rescan(cx);
        self.changed(cx);
        Ok(())
    }

    /// The plugin's settings changed in Settings: it gets the new values and `settings-changed`.
    pub fn settings_changed(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(state) = self.plugin(id) else {
            return;
        };
        let values = settings_values(&state.entry, cx);
        if let Some(instance) = self.instances.get(id) {
            instance.set_settings(values);
            instance.send_event(Event::SettingsChanged);
        }
    }

    /// The window's project root changed.
    pub fn set_root(&mut self, root: Option<PathBuf>, cx: &mut Context<Self>) {
        self.root = root.clone();
        let text = root.as_deref().map(path_text);
        for instance in self.instances.values() {
            instance.set_project_root(root.clone());
            instance.send_event(Event::ProjectChanged(text.clone()));
        }
        cx.notify();
    }

    /// Follows a document: its opening, edits, selections, saves and closing become plugin
    /// events.
    pub fn register(&mut self, editor: &Entity<Editor>, cx: &mut Context<Self>) {
        let entity = editor.entity_id();
        if self.editors.contains_key(&entity) {
            return;
        }
        let id = entity.as_u64();
        let events = cx.subscribe(
            editor,
            move |this, editor, event: &EditorEvent, cx| match event {
                EditorEvent::Edited => this.broadcast(Event::EditorChanged(id)),
                EditorEvent::SelectionsChanged => this.broadcast(Event::SelectionChanged(id)),
                EditorEvent::Saved => {
                    // Save As moves the document: the plugins' problems follow its file.
                    this.apply_published(&editor, cx);
                    let info = editor_info(&editor, cx);
                    this.broadcast(Event::EditorSaved(info))
                }
                EditorEvent::SaveFailed(_) => {}
            },
        );
        let release = cx.observe_release(editor, move |this, _, _| {
            this.editors.remove(&entity);
            this.editor_handles.remove(&id);
            if this.active_editor == Some(id) {
                this.active_editor = None;
            }
            this.broadcast(Event::EditorClosed(id));
        });
        self.editors.insert(entity, [events, release]);
        self.editor_handles.insert(id, editor.downgrade());
        // The editor's context menu shows the plugins' items.
        let store = cx.weak_entity();
        editor.update(cx, |editor, _| editor.plugins = Some(store));
        // Problems the plugins published for the file before it was opened.
        self.apply_published(editor, cx);
        let info = editor_info(editor, cx);
        self.broadcast(Event::EditorOpened(info));
    }

    /// The active tab changed: its document, or none.
    pub(crate) fn set_active_editor(&mut self, editor: Option<&Entity<Editor>>, cx: &App) {
        let id = editor.map(|editor| editor.entity_id().as_u64());
        if id == self.active_editor {
            return;
        }
        self.active_editor = id;
        let info = editor.map(|editor| editor_info(editor, cx));
        self.broadcast(Event::ActiveEditorChanged(info));
    }

    /// The window showed or hid a plugin's tool window: the plugin hears of it.
    pub(crate) fn tool_window_visibility(&mut self, key: ToolKey, shown: bool) {
        if shown {
            self.visible_tools.insert(key);
        } else {
            self.visible_tools.remove(&key);
        }
        let Some(tool) = self.tool_window(key) else {
            return;
        };
        let window = tool.id.to_string();
        let event = if shown {
            Event::ToolWindowShown(window)
        } else {
            Event::ToolWindowHidden(window)
        };
        let plugin = tool.plugin.clone();
        self.send_to(&plugin, event);
    }

    /// The tool windows of the plugins that are on, in the launchpad's order.
    pub fn tool_windows(&self) -> &[ToolWindow] {
        &self.tool_windows
    }

    pub fn tool_window(&self, key: ToolKey) -> Option<&ToolWindow> {
        self.tool_windows.iter().find(|window| window.key == key)
    }

    /// The status bar items the plugins show.
    pub fn status_items(&self) -> &[StatusItem] {
        &self.status_items
    }

    /// The running plugins' commands for the palette.
    pub fn palette_commands(&self) -> Vec<PaletteCommand> {
        let mut commands = Vec::new();
        for state in &self.plugins {
            if !self.instances.contains_key(state.id()) {
                continue;
            }
            let manifest = &state.entry.manifest;
            for command in &manifest.commands {
                let category = command.category.as_deref().unwrap_or(&manifest.name);
                commands.push(PaletteCommand {
                    action: RunCommand {
                        plugin: state.id().to_string().into(),
                        command: command.id.clone().into(),
                        origin: Some(CommandOrigin::new(CommandSource::Palette)),
                    },
                    category: state.tr(category).to_string(),
                    title: state.tr(&command.title).to_string(),
                    enabled: !self
                        .disabled_commands
                        .contains(&(Arc::from(state.id()), command.id.clone())),
                });
            }
        }
        commands
    }

    /// The tool windows for the palette.
    pub fn palette_tool_windows(&self) -> Vec<PaletteToolWindow> {
        self.tool_windows
            .iter()
            .filter_map(|tool| {
                let state = self.plugin(&tool.plugin)?;
                Some(PaletteToolWindow {
                    action: ToggleToolWindow {
                        plugin: tool.plugin.clone(),
                        window: tool.id.clone(),
                    },
                    category: state.name().to_string(),
                    title: trf("Toggle {0} Window", &[&tool.title]),
                })
            })
            .collect()
    }

    /// Runs a plugin's command, unless the plugin isn't running or grayed the command out:
    /// `origin` says where from (none — its keys) and what on.
    pub fn run_command(
        &mut self,
        plugin: &str,
        command: &str,
        origin: Option<&CommandOrigin>,
        cx: &mut Context<Self>,
    ) {
        let context = self.command_context(origin, cx);
        let Some(instance) = self.instances.get(plugin) else {
            return;
        };
        let known = self
            .plugin(plugin)
            .is_some_and(|state| state.entry.manifest.command(command).is_some());
        if known
            && !self
                .disabled_commands
                .contains(&(Arc::from(plugin), command.to_string()))
        {
            instance.run_command(command, context);
        }
    }

    /// What a command acts on, as of now: where it was run from, the active document (a tab's
    /// menu activates its tab first) and its selections, the paths of the menu — or the
    /// document's file. The tree's menu acts on its rows, not on a document.
    fn command_context(&self, origin: Option<&CommandOrigin>, cx: &App) -> CommandContext {
        let source = origin.map_or(CommandSource::Keys, |origin| origin.source);
        let editor = match source {
            CommandSource::TreeMenu => None,
            _ => self
                .active_editor
                .and_then(|id| self.editor_handles.get(&id)?.upgrade()),
        };
        let info = editor.as_ref().map(|editor| editor_info(editor, cx));
        let selections = editor
            .as_ref()
            .map(|editor| selections_of(editor.read(cx)))
            .unwrap_or_default();
        let mut paths: Vec<String> = origin
            .map(|origin| origin.paths.iter().map(|path| path_text(path)).collect())
            .unwrap_or_default();
        if paths.is_empty()
            && let Some(path) = info.as_ref().and_then(|info| info.path.clone())
        {
            paths.push(path);
        }
        CommandContext {
            source,
            editor: info,
            selections,
            paths,
        }
    }

    /// Whether the plugin runs and hasn't grayed the command out: its menu items show then.
    pub fn command_enabled(&self, plugin: &str, command: &str) -> bool {
        self.instances.contains_key(plugin)
            && !self
                .disabled_commands
                .contains(&(Arc::from(plugin), command.to_string()))
    }

    /// The window's Git and language servers (part 8.2): `git-changed` comes when a repository's
    /// changes, branch, HEAD or operation do (the store notifies far more often), and
    /// `diagnostics-changed` when a server publishes. Called again when the window makes new ones
    /// (another project root).
    pub(crate) fn set_window_parts(
        &mut self,
        git: Entity<crate::git::GitStore>,
        lsp: Entity<crate::lsp::LspStore>,
        cx: &mut Context<Self>,
    ) {
        self.git_state = Some(crate::plugin_calls::git_state(git.read(cx)));
        self._window_parts = vec![
            cx.observe(&git, |this, git, cx| {
                let state = crate::plugin_calls::git_state(git.read(cx));
                if this.git_state != Some(state) {
                    this.git_state = Some(state);
                    this.broadcast(Event::GitChanged);
                }
            }),
            cx.subscribe(
                &lsp,
                |this, _, event: &crate::lsp::DiagnosticsChanged, cx| {
                    this.diagnostics_changed(event.0.iter().cloned(), cx)
                },
            ),
        ];
    }

    // --- The plugins' problems (part 8.2) ---

    /// A plugin's problems of a file (absolute), replacing those it published before; an empty
    /// list clears them. They go into the file's document now if it is open.
    pub(crate) fn publish_diagnostics(
        &mut self,
        plugin: &str,
        path: PathBuf,
        mut diagnostics: Vec<ApiDiagnostic>,
        cx: &mut Context<Self>,
    ) {
        // A late call of a plugin that has stopped since.
        if !self.instances.contains_key(plugin) {
            return;
        }
        if diagnostics.len() > MAX_PUBLISHED {
            self.log(
                plugin,
                Level::Warn,
                &format!(
                    "diagnostics.publish: {} problems for {}; only the first {MAX_PUBLISHED} are shown",
                    diagnostics.len(),
                    path.display()
                ),
            );
            diagnostics.truncate(MAX_PUBLISHED);
        }
        let files = self.published.entry(Arc::from(plugin)).or_default();
        if diagnostics.is_empty() {
            files.remove(&path);
        } else {
            files.insert(path.clone(), diagnostics);
        }
        let target = crate::navigation::canonical(&path);
        for editor in self.live_editors() {
            let on_path = editor
                .read(cx)
                .document
                .path()
                .is_some_and(|path| crate::navigation::canonical(path) == target);
            if on_path {
                self.apply_plugin_diagnostics(plugin, &editor, cx);
            }
        }
        self.diagnostics_changed([path], cx);
    }

    /// Takes away every problem a plugin published: it asked, or it stopped.
    pub(crate) fn clear_diagnostics(&mut self, plugin: &str, cx: &mut Context<Self>) {
        let Some(files) = self.published.remove(plugin) else {
            return;
        };
        let owner = crate::diagnostics::plugin_owner(plugin);
        for editor in self.live_editors() {
            editor.update(cx, |editor, cx| {
                if editor.diagnostics.iter().any(|d| d.owner == owner) {
                    editor.diagnostics.clear_owner(owner);
                    cx.notify();
                }
            });
        }
        self.diagnostics_changed(files.into_keys(), cx);
    }

    /// The problems the plugins published for files none of `open` (canonical paths) is: the
    /// file and its problems (several plugins' together, each with its source).
    pub(crate) fn published_elsewhere(&self, open: &[PathBuf]) -> Vec<(PathBuf, Vec<ApiDiagnostic>)> {
        let mut files: Vec<(PathBuf, Vec<ApiDiagnostic>)> = Vec::new();
        for (plugin, published) in &self.published {
            let name = self.plugin(plugin).map_or(plugin.to_string(), |state| state.name().to_string());
            for (path, diagnostics) in published {
                if open.contains(&crate::navigation::canonical(path)) {
                    continue;
                }
                let diagnostics = diagnostics.iter().cloned().map(|mut diagnostic| {
                    diagnostic.source.get_or_insert_with(|| name.clone());
                    diagnostic
                });
                match files.iter_mut().find(|(known, _)| known == path) {
                    Some((_, known)) => known.extend(diagnostics),
                    None => files.push((path.clone(), diagnostics.collect())),
                }
            }
        }
        files
    }

    /// The documents the store knows that are still open.
    fn live_editors(&self) -> Vec<Entity<Editor>> {
        self.editor_handles
            .values()
            .filter_map(WeakEntity::upgrade)
            .collect()
    }

    /// Puts every plugin's problems of the document's file into it (and takes away those of a file
    /// it no longer is: Save As).
    fn apply_published(&self, editor: &Entity<Editor>, cx: &mut App) {
        let plugins: Vec<Arc<str>> = self.published.keys().cloned().collect();
        for plugin in plugins {
            self.apply_plugin_diagnostics(&plugin, editor, cx);
        }
    }

    /// Puts a plugin's problems of the document's file into the document under the plugin's owner,
    /// in place of those it had.
    fn apply_plugin_diagnostics(&self, plugin: &str, editor: &Entity<Editor>, cx: &mut App) {
        let owner = crate::diagnostics::plugin_owner(plugin);
        let source = self
            .plugin(plugin)
            .map_or(plugin.to_string(), |state| state.name().to_string());
        let target = editor.read(cx).document.path().map(crate::navigation::canonical);
        let published = target.and_then(|target| {
            self.published
                .get(plugin)?
                .iter()
                .find(|(path, _)| crate::navigation::canonical(path) == target)
                .map(|(_, diagnostics)| diagnostics.clone())
        });
        editor.update(cx, |editor, cx| {
            let had = editor.diagnostics.iter().any(|d| d.owner == owner);
            match published {
                Some(published) => {
                    let items = crate::plugin_calls::to_editor_diagnostics(
                        editor.document.text(),
                        &published,
                        &source,
                    );
                    editor.diagnostics.set(owner, items);
                }
                None if had => editor.diagnostics.clear_owner(owner),
                None => return,
            }
            cx.notify();
        });
    }

    /// The problems of `paths` changed: the plugins hear of them together, a moment later.
    fn diagnostics_changed(&mut self, paths: impl IntoIterator<Item = PathBuf>, cx: &mut Context<Self>) {
        if self.instances.is_empty() {
            return;
        }
        self.changed_diagnostics.extend(paths);
        if self.diagnostics_flush.is_some() || self.changed_diagnostics.is_empty() {
            return;
        }
        self.diagnostics_flush = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DIAGNOSTICS_DEBOUNCE).await;
            this.update(cx, |this, _| {
                this.diagnostics_flush = None;
                let paths: Vec<String> = std::mem::take(&mut this.changed_diagnostics)
                    .iter()
                    .map(|path| path_text(path))
                    .collect();
                if !paths.is_empty() {
                    this.broadcast(Event::DiagnosticsChanged(paths));
                }
            })
            .ok();
        }));
    }

    /// Delivers an event to one plugin, if it runs.
    pub(crate) fn send_to(&self, plugin: &str, event: Event) {
        if let Some(instance) = self.instances.get(plugin) {
            instance.send_event(event);
        }
    }

    /// Why a stopped plugin stopped: the line and the whole story.
    pub fn stop_reason(&self, id: &str) -> Option<(SharedString, SharedString)> {
        match &self.plugin(id)?.status {
            PluginStatus::Stopped { error, details } => Some((error.clone(), details.clone())),
            _ => None,
        }
    }

    // --- Starting and stopping ---

    fn index(&self, id: &str) -> Option<usize> {
        self.plugins.iter().position(|plugin| plugin.id() == id)
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        self.refresh_contributions(cx);
        cx.emit(PluginStoreEvent::Changed);
        cx.notify();
    }

    /// The plugins whose declarative contributions are in place: turned on and able to run (a
    /// plugin with code adds them whether its instance runs or stopped).
    fn contributing(&self) -> Vec<Arc<PluginEntry>> {
        self.plugins
            .iter()
            .filter(|plugin| {
                !matches!(
                    plugin.status,
                    PluginStatus::Disabled | PluginStatus::Unavailable(_)
                )
            })
            .map(|plugin| plugin.entry.clone())
            .collect()
    }

    /// Puts the languages, servers, themes and icons of the turned-on plugins in place (stage
    /// 8.3); what couldn't be read goes into the plugin's log.
    fn refresh_contributions(&mut self, cx: &mut Context<Self>) {
        let plugins = self.contributing();
        for (id, problem) in crate::contributions::refresh(&plugins, cx) {
            if let Some(plugin) = self.plugins.iter().find(|plugin| plugin.id() == id) {
                plugin.log.write(Level::Warn, &problem);
            }
        }
    }

    fn bump_generation(&mut self, id: &str) -> u64 {
        let generation = self.generations.entry(Arc::from(id)).or_default();
        *generation += 1;
        *generation
    }

    fn generation(&self, id: &str) -> u64 {
        self.generations.get(id).copied().unwrap_or_default()
    }

    /// Finds the plugins again (an install, a removal): new ones are added and started if on,
    /// gone ones stopped and removed, changed ones take their new manifest.
    fn rescan(&mut self, cx: &mut Context<Self>) {
        let dev = settings::dev_plugins(cx);
        let scan = flux_plugin::registry::scan(crate::bundled::plugins(), &dev);
        self.scan_errors = scan.errors;
        let found: HashSet<String> = scan
            .plugins
            .iter()
            .map(|entry| entry.id().to_string())
            .collect();
        let gone: Vec<String> = self
            .plugins
            .iter()
            .map(|plugin| plugin.id().to_string())
            .filter(|id| !found.contains(id))
            .collect();
        for id in &gone {
            self.stop(id, cx);
            self.remove_contributions(id, cx);
            icons::unregister_plugin_files(id);
        }
        let mut old: HashMap<String, PluginState> = std::mem::take(&mut self.plugins)
            .into_iter()
            .map(|state| (state.id().to_string(), state))
            .collect();
        let mut to_start = Vec::new();
        for entry in scan.plugins {
            let id = entry.id().to_string();
            icons::register_plugin_files(&id, entry.files.clone());
            let state = match old.remove(&id) {
                Some(mut state) => {
                    let replaced = state.entry.source != entry.source
                        || state.entry.manifest != entry.manifest
                        || state.entry.files.dir() != entry.files.dir();
                    if replaced {
                        state.entry = Arc::new(entry);
                        if let Some(problem) = &state.entry.problem {
                            state.status = PluginStatus::Unavailable(problem.clone().into());
                        }
                    }
                    state
                }
                None => {
                    let enabled = settings::plugin_enabled(&id, cx);
                    let status = match &entry.problem {
                        Some(problem) => PluginStatus::Unavailable(problem.clone().into()),
                        None if !enabled => PluginStatus::Disabled,
                        None => {
                            to_start.push(id.clone());
                            PluginStatus::Starting
                        }
                    };
                    PluginState {
                        log: PluginLog::open(&id),
                        entry: Arc::new(entry),
                        status,
                    }
                }
            };
            self.plugins.push(state);
        }
        for id in to_start {
            self.start(&id, true, cx);
        }
        self.changed(cx);
    }

    /// Starts a plugin (stopping its running instance first): registers its notification group,
    /// makes its tool windows and binds its keys; a plugin under development with a cargo project
    /// is built first when `build`.
    fn start(&mut self, id: &str, build: bool, cx: &mut Context<Self>) {
        let Some(index) = self.index(id) else {
            return;
        };
        self.stop(id, cx);
        self.bump_generation(id);
        let entry = self.plugins[index].entry.clone();
        self.plugins[index].status = PluginStatus::Starting;
        self.register_group(index, cx);
        self.sync_tool_windows(id, cx);
        self.bind_keys(index, cx);
        if let Some(previous) = self.stop_notifications.remove(id) {
            self.with_center(cx, |center, cx| center.expire(previous, cx));
        }
        let cargo = entry.source == PluginSource::Dev
            && entry
                .files
                .dir()
                .is_some_and(flux_plugin::dev::is_cargo_project);
        if build && cargo {
            self.build_then_launch(id, cx);
        } else {
            self.launch(id, cx);
        }
        cx.notify();
    }

    /// Starts the plugin's instance; a plugin without code just runs.
    fn launch(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(index) = self.index(id) else {
            return;
        };
        let state = &self.plugins[index];
        let entry = state.entry.clone();
        if entry.source == PluginSource::Dev {
            self.dev_files
                .insert(Arc::from(id), declarative_files(&entry));
        }
        if entry.manifest.wasm.is_none() {
            self.plugins[index].status = PluginStatus::Running;
            return self.changed(cx);
        }
        let key: Arc<str> = Arc::from(id);
        if entry.source == PluginSource::Dev {
            self.dev_components
                .insert(key.clone(), flux_plugin::dev::component_mtime(&entry));
        }
        let config = InstanceConfig {
            project_root: self.root.clone(),
            language: crate::i18n::lang_code().to_string(),
            settings: settings_values(&entry, cx),
            messages: self.messages.clone(),
            log: state.log.clone(),
            entry,
        };
        self.instances.insert(key, Instance::start(config));
        self.show_placeholders(id, None, cx);
    }

    /// Stops the plugin's instance, if it runs, and takes its status bar items away; its tool
    /// windows stay (a restart or a reload keeps them open).
    fn stop(&mut self, id: &str, cx: &mut Context<Self>) {
        self.bump_generation(id);
        self.building.remove(id);
        if let Some(instance) = self.instances.remove(id) {
            instance.stop();
            cx.emit(PluginStoreEvent::PluginStopped(Arc::from(id)));
        }
        self.clear_diagnostics(id, cx);
        let before = self.status_items.len();
        self.status_items.retain(|item| item.plugin != id);
        self.disabled_commands.retain(|(plugin, _)| &**plugin != id);
        if self.status_items.len() != before {
            cx.notify();
        }
    }

    /// The plugin is off or gone: its tool windows and status bar items go.
    fn remove_contributions(&mut self, id: &str, cx: &mut Context<Self>) {
        let gone: Vec<ToolKey> = self
            .tool_windows
            .iter()
            .filter(|tool| tool.plugin == id)
            .map(|tool| tool.key)
            .collect();
        for key in &gone {
            self._view_subscriptions.remove(key);
            self.placeholders.remove(key);
            self.visible_tools.remove(key);
        }
        self.tool_windows.retain(|tool| tool.plugin != id);
        self.status_items.retain(|item| item.plugin != id);
        self.dev_components.remove(id);
        self.dev_files.remove(id);
        cx.notify();
    }

    /// Re-reads a plugin under development from its folder (the manifest may have changed) and
    /// starts it again, built first when `build`.
    fn reload_from_disk(&mut self, id: &str, build: bool, cx: &mut Context<Self>) {
        let Some(index) = self.index(id) else {
            return;
        };
        if !settings::plugin_enabled(id, cx) {
            return;
        }
        let Some(dir) = self.plugins[index].entry.files.dir().map(Path::to_path_buf) else {
            self.start(id, false, cx);
            return self.changed(cx);
        };
        match flux_plugin::registry::load_dir(&dir, PluginSource::Dev) {
            Ok(entry) if entry.id() == id => {
                icons::register_plugin_files(id, entry.files.clone());
                self.plugins[index].entry = Arc::new(entry);
                self.start(id, build, cx);
            }
            Ok(entry) => {
                // The folder now holds another plugin: the list is read again.
                self.plugins[index]
                    .log
                    .write(Level::Warn, &format!("The folder now holds {}", entry.id()));
                self.stop(id, cx);
                self.rescan(cx);
            }
            Err(error) => {
                self.stop(id, cx);
                self.plugins[index].status = PluginStatus::Stopped {
                    error: error.clone().into(),
                    details: SharedString::default(),
                };
                self.plugins[index].log.write(Level::Error, &error);
                self.show_placeholders(id, Some(&error), cx);
            }
        }
        self.changed(cx);
    }

    /// Builds a plugin under development in the background, then starts it.
    fn build_then_launch(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(index) = self.index(id) else {
            return;
        };
        let state = &self.plugins[index];
        let Some(dir) = state.entry.files.dir().map(Path::to_path_buf) else {
            return self.launch(id, cx);
        };
        let key: Arc<str> = Arc::from(id);
        self.building.insert(key.clone());
        // Only a component newer than this one (the author's own build) reloads the plugin: a
        // failed build doesn't fall back to the old one by itself.
        self.dev_components
            .insert(key.clone(), flux_plugin::dev::component_mtime(&state.entry));
        let log = state.log.clone();
        let name = state.name().to_string();
        let progress = self.post(
            Notification::info(trf("Building «{0}»…", &[&name]))
                .progress(Progress::Indeterminate)
                .group(NotificationGroup::Plugins),
            cx,
        );
        self.show_building(id, cx);
        let generation = self.generation(id);
        let build = cx.background_spawn(async move { flux_plugin::dev::build(&dir, &log) });
        cx.spawn(async move |this, cx| {
            let result = build.await;
            this.update(cx, |this, cx| {
                if let Some(progress) = progress {
                    this.with_center(cx, |center, cx| center.remove(progress, cx));
                }
                if this.generation(&key) != generation {
                    return;
                }
                this.building.remove(&key);
                this.build_finished(&key, result, cx);
            })
            .ok();
        })
        .detach();
    }

    fn build_finished(&mut self, id: &str, result: Result<(), String>, cx: &mut Context<Self>) {
        let Some(index) = self.index(id) else {
            return;
        };
        match result {
            Ok(()) => {
                // The build may have changed the manifest's component (its first build).
                if let Some(dir) = self.plugins[index].entry.files.dir().map(Path::to_path_buf)
                    && let Ok(entry) = flux_plugin::registry::load_dir(&dir, PluginSource::Dev)
                    && entry.id() == id
                {
                    self.plugins[index].entry = Arc::new(entry);
                }
                self.launch(id, cx);
            }
            Err(output) => {
                // The compiler's first error says the most; the whole output is in Details.
                let error: SharedString = output
                    .lines()
                    .map(str::trim)
                    .find(|line| line.starts_with("error"))
                    .map_or_else(|| tr("The build failed").to_string(), str::to_string)
                    .into();
                self.plugins[index].status = PluginStatus::Stopped {
                    error: error.clone(),
                    details: output.into(),
                };
                self.show_placeholders(id, Some(&error), cx);
                self.report_stop(id, true, cx);
            }
        }
        self.changed(cx);
    }

    /// Looks at the components of the running plugins under development: a newer one (the
    /// author built it) is reloaded.
    fn check_dev_components(&mut self, cx: &mut Context<Self>) {
        let watched = self.plugins.iter().filter(|plugin| {
            plugin.source() == PluginSource::Dev
                && !self.building.contains(plugin.id())
                && matches!(
                    plugin.status,
                    PluginStatus::Running | PluginStatus::Stopped { .. }
                )
        });
        let changed: Vec<(String, &'static str)> = watched
            .filter_map(|plugin| {
                let now = flux_plugin::dev::component_mtime(&plugin.entry);
                let component = now.is_some()
                    && self
                        .dev_components
                        .get(plugin.id())
                        .is_none_or(|known| *known != now);
                // A query, a theme or the manifest edited: the plugin is read again (stage 8.3).
                let files = self
                    .dev_files
                    .get(plugin.id())
                    .is_some_and(|known| *known != declarative_files(&plugin.entry));
                let why = match (component, files) {
                    (true, _) => "The component changed: reloading",
                    (false, true) => "Its files changed: reloading",
                    (false, false) => return None,
                };
                Some((plugin.id().to_string(), why))
            })
            .collect();
        for (id, why) in changed {
            let Some(index) = self.index(&id) else {
                continue;
            };
            let name = self.plugins[index].name().to_string();
            self.plugins[index].log.write(Level::Info, why);
            self.reload_from_disk(&id, false, cx);
            self.post(
                Notification::success(trf("Plugin «{0}» reloaded", &[&name]))
                    .group(NotificationGroup::Plugins),
                cx,
            );
        }
    }

    // --- Contributions ---

    /// The plugin's notification group, with its name and default display.
    fn register_group(&mut self, index: usize, cx: &mut Context<Self>) {
        let state = &self.plugins[index];
        let default_display = match state.entry.manifest.notifications {
            NotificationDisplay::Balloon => Display::Balloon,
            NotificationDisplay::Sticky => Display::StickyBalloon,
            NotificationDisplay::Log => Display::LogOnly,
            NotificationDisplay::Hidden => Display::Hidden,
        };
        crate::notification_center::register_group(
            GroupInfo {
                group: NotificationGroup::Plugin(state.id().to_string().into()),
                title: state.name().to_string().into(),
                default_display,
            },
            cx,
        );
    }

    /// Makes the tool windows the manifest declares (keeping those that exist: a reload keeps an
    /// open window open) and drops the ones it no longer has.
    fn sync_tool_windows(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(index) = self.index(id) else {
            return;
        };
        let entry = self.plugins[index].entry.clone();
        let manifest = &entry.manifest;
        let gone: Vec<ToolKey> = self
            .tool_windows
            .iter()
            .filter(|tool| tool.plugin == id && manifest.tool_window(&tool.id).is_none())
            .map(|tool| tool.key)
            .collect();
        for key in &gone {
            self._view_subscriptions.remove(key);
            self.placeholders.remove(key);
        }
        self.tool_windows.retain(|tool| !gone.contains(&tool.key));
        for spec in &manifest.tool_windows {
            let title: SharedString = self.plugins[index].tr(&spec.title).to_string().into();
            let icon = spec
                .icon
                .as_deref()
                .map(|icon| icons::plugin_asset(id, icon));
            if let Some(tool) = self
                .tool_windows
                .iter_mut()
                .find(|tool| tool.plugin == id && tool.id == spec.id)
            {
                tool.title = title;
                tool.icon = icon;
                continue;
            }
            let key = ToolKey(self.next_tool);
            self.next_tool += 1;
            let plugin: SharedString = id.to_string().into();
            let window: SharedString = spec.id.clone().into();
            let width = self.width.clone();
            let view = cx.new(|cx| {
                PluginView::new(plugin.clone(), window.clone(), title.clone(), width, cx)
            });
            let subscription = cx.subscribe(&view, move |this, _, event, cx| {
                this.on_view_event(key, event, cx)
            });
            self._view_subscriptions.insert(key, subscription);
            self.tool_windows.push(ToolWindow {
                key,
                plugin,
                id: window,
                title,
                icon,
                view,
            });
        }
        // The launchpad's order: the plugins' order, then the manifest's.
        let tools = std::mem::take(&mut self.tool_windows);
        let order = |tool: &ToolWindow| {
            let plugin = self.index(&tool.plugin).unwrap_or(usize::MAX);
            let window = self
                .plugin(&tool.plugin)
                .and_then(|state| {
                    state
                        .entry
                        .manifest
                        .tool_windows
                        .iter()
                        .position(|spec| spec.id == tool.id)
                })
                .unwrap_or(usize::MAX);
            (plugin, window)
        };
        let mut keyed: Vec<((usize, usize), ToolWindow)> =
            tools.into_iter().map(|tool| (order(&tool), tool)).collect();
        keyed.sort_by_key(|(order, _)| *order);
        self.tool_windows = keyed.into_iter().map(|(_, tool)| tool).collect();
    }

    fn on_view_event(&mut self, key: ToolKey, event: &PluginViewEvent, cx: &mut Context<Self>) {
        match event {
            PluginViewEvent::Input(input) => {
                let Some(tool) = self.tool_window(key) else {
                    return;
                };
                let plugin = tool.plugin.clone();
                if self.placeholders.contains(&key) {
                    if input.element == RESTART_ELEMENT && input.event == UiEvent::Clicked {
                        self.restart(&plugin, cx);
                    }
                    return;
                }
                self.send_to(&plugin, Event::Ui(input.clone()));
            }
            PluginViewEvent::Hide => cx.emit(PluginStoreEvent::HideToolWindow(key)),
        }
    }

    /// The tool windows of a plugin that isn't running show why (a stopped plugin offers a
    /// restart); `None` — they wait for the plugin's own view.
    fn show_placeholders(&mut self, id: &str, stopped: Option<&str>, cx: &mut Context<Self>) {
        let name = self.plugin(id).map(|state| state.name().to_string());
        for tool in &self.tool_windows {
            if tool.plugin != id {
                continue;
            }
            match (stopped, &name) {
                (Some(error), Some(name)) => {
                    let view = stopped_view(name, error);
                    tool.view.update(cx, |view_, cx| view_.set_view(view, cx));
                    self.placeholders.insert(tool.key);
                }
                _ => {
                    if self.placeholders.remove(&tool.key) {
                        tool.view.update(cx, |view, cx| {
                            view.set_view(
                                View {
                                    elements: Vec::new(),
                                },
                                cx,
                            )
                        });
                    }
                }
            }
        }
    }

    /// The tool windows of a plugin being built say so.
    fn show_building(&mut self, id: &str, cx: &mut Context<Self>) {
        for tool in &self.tool_windows {
            if tool.plugin == id {
                tool.view
                    .update(cx, |view, cx| view.set_view(building_view(), cx));
                self.placeholders.insert(tool.key);
            }
        }
    }

    /// Binds the keys of the plugin's commands and tool windows, once per window's lifetime.
    fn bind_keys(&mut self, index: usize, cx: &mut Context<Self>) {
        let state = &self.plugins[index];
        let plugin: SharedString = state.id().to_string().into();
        let log = state.log.clone();
        let manifest = state.entry.manifest.clone();
        for command in &manifest.commands {
            if let Some(keys) = &command.keys {
                let action = RunCommand {
                    plugin: plugin.clone(),
                    command: command.id.clone().into(),
                    origin: None,
                };
                let what = format!("command \"{}\"", command.id);
                bind_plugin_keys(keys, action, &what, &log, cx);
            }
        }
        for window in &manifest.tool_windows {
            if let Some(keys) = &window.keys {
                let action = ToggleToolWindow {
                    plugin: plugin.clone(),
                    window: window.id.clone().into(),
                };
                let what = format!("tool window \"{}\"", window.id);
                bind_plugin_keys(keys, action, &what, &log, cx);
            }
        }
    }

    // --- Messages of the instances ---

    fn handle_message(&mut self, message: PluginMessage, cx: &mut Context<Self>) {
        let plugin = message.plugin;
        // A message of an instance that was stopped since (a reload): it has nobody to talk to.
        if !self.instances.contains_key(&plugin) {
            return;
        }
        match message.kind {
            MessageKind::Started => {
                if let Some(index) = self.index(&plugin) {
                    self.plugins[index].status = PluginStatus::Running;
                    // A window on screen while the plugin (re)started: it fills it now (TODO
                    // searches only while its window is shown).
                    let shown: Vec<String> = self
                        .tool_windows
                        .iter()
                        .filter(|tool| {
                            tool.plugin == *plugin && self.visible_tools.contains(&tool.key)
                        })
                        .map(|tool| tool.id.to_string())
                        .collect();
                    for window in shown {
                        self.send_to(&plugin, Event::ToolWindowShown(window));
                    }
                    self.changed(cx);
                }
            }
            MessageKind::Logged => cx.notify(),
            MessageKind::Stopped { error, details } => {
                self.instances.remove(&plugin);
                cx.emit(PluginStoreEvent::PluginStopped(plugin.clone()));
                self.clear_diagnostics(&plugin, cx);
                self.bump_generation(&plugin);
                self.status_items.retain(|item| item.plugin != *plugin);
                let Some(index) = self.index(&plugin) else {
                    return;
                };
                self.plugins[index].status = PluginStatus::Stopped {
                    error: error.clone().into(),
                    details: details.into(),
                };
                self.show_placeholders(&plugin, Some(&error), cx);
                self.report_stop(&plugin, false, cx);
                self.changed(cx);
            }
            MessageKind::Call(call) => self.handle_store_call(plugin, call, cx),
        }
    }

    /// Carries out a call about the store's own state; the rest goes to the window.
    fn handle_store_call(&mut self, plugin: Arc<str>, call: HostCall, cx: &mut Context<Self>) {
        match call {
            HostCall::Notify { id, notification } => {
                let notification = plugin_notification(&plugin, &notification);
                if let Some(center_id) = self.post(notification, cx) {
                    self.notification_ids.insert((plugin, id), center_id);
                }
            }
            HostCall::UpdateNotification { id, notification } => {
                let Some(&center_id) = self.notification_ids.get(&(plugin.clone(), id)) else {
                    return;
                };
                let new = plugin_notification(&plugin, &notification);
                self.with_center(cx, |center, cx| {
                    center.update(
                        center_id,
                        |notification| {
                            let progress = notification.progress;
                            *notification = new;
                            notification.progress = progress;
                        },
                        cx,
                    )
                });
            }
            HostCall::SetProgress { id, progress } => {
                let Some(&center_id) = self.notification_ids.get(&(plugin, id)) else {
                    return;
                };
                let progress = progress.map(|progress| match progress {
                    ApiProgress::Indeterminate => Progress::Indeterminate,
                    ApiProgress::Fraction(fraction) => Progress::Fraction(fraction.clamp(0., 1.)),
                });
                self.with_center(cx, |center, cx| {
                    center.set_progress(center_id, progress, cx)
                });
            }
            HostCall::ExpireNotification { id } => {
                let Some(&center_id) = self.notification_ids.get(&(plugin, id)) else {
                    return;
                };
                self.with_center(cx, |center, cx| center.expire(center_id, cx));
            }
            HostCall::RemoveNotification { id } => {
                let Some(center_id) = self.notification_ids.remove(&(plugin, id)) else {
                    return;
                };
                self.with_center(cx, |center, cx| center.remove(center_id, cx));
            }
            HostCall::SetCommandEnabled { command, enabled } => {
                let key = (plugin, command);
                if enabled {
                    self.disabled_commands.remove(&key);
                } else {
                    self.disabled_commands.insert(key);
                }
            }
            HostCall::SetStatusItem { id, item } => self.set_status_item(&plugin, id, item, cx),
            HostCall::SetView { window, view } => {
                let Some(tool) = self
                    .tool_windows
                    .iter()
                    .find(|tool| tool.plugin == *plugin && tool.id == window)
                else {
                    return self.log(&plugin, Level::Warn, &undeclared("tool window", &window));
                };
                self.placeholders.remove(&tool.key);
                tool.view.update(cx, |tool, cx| tool.set_view(view, cx));
            }
            HostCall::ShowToolWindow { window } | HostCall::HideToolWindow { window }
                if self.tool_key(&plugin, &window).is_none() =>
            {
                self.log(&plugin, Level::Warn, &undeclared("tool window", &window))
            }
            HostCall::ShowToolWindow { window } => {
                if let Some(key) = self.tool_key(&plugin, &window) {
                    cx.emit(PluginStoreEvent::ShowToolWindow(key));
                }
            }
            HostCall::HideToolWindow { window } => {
                if let Some(key) = self.tool_key(&plugin, &window) {
                    cx.emit(PluginStoreEvent::HideToolWindow(key));
                }
            }
            call @ (HostCall::Ask { .. }
            | HostCall::AskText { .. }
            | HostCall::Editor { .. }
            | HostCall::Window { .. }) => cx.emit(PluginStoreEvent::Call { plugin, call }),
        }
    }

    fn tool_key(&self, plugin: &str, window: &str) -> Option<ToolKey> {
        self.tool_windows
            .iter()
            .find(|tool| tool.plugin == plugin && tool.id == window)
            .map(|tool| tool.key)
    }

    fn set_status_item(
        &mut self,
        plugin: &str,
        id: String,
        item: Option<flux_plugin::api::status_bar::StatusItem>,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.plugin(plugin) else {
            return;
        };
        if !state
            .entry
            .manifest
            .status_items
            .iter()
            .any(|spec| spec.id == id)
        {
            return self.log(plugin, Level::Warn, &undeclared("status item", &id));
        }
        self.status_items
            .retain(|item| !(item.plugin == plugin && item.id == id));
        if let Some(item) = item {
            self.status_items.push(StatusItem {
                plugin: plugin.to_string().into(),
                id: id.into(),
                text: item.text.into(),
                tooltip: item.tooltip.map(Into::into),
                command: item.command.map(|command| RunCommand {
                    plugin: plugin.to_string().into(),
                    command: command.into(),
                    origin: Some(CommandOrigin::new(CommandSource::StatusBar)),
                }),
            });
        }
        // The plugins' order, then the manifest's.
        let rank = |item: &StatusItem, store: &Self| {
            let plugin = store.index(&item.plugin).unwrap_or(usize::MAX);
            let order = store
                .plugin(&item.plugin)
                .and_then(|state| {
                    state
                        .entry
                        .manifest
                        .status_items
                        .iter()
                        .position(|spec| spec.id == item.id)
                })
                .unwrap_or(usize::MAX);
            (plugin, order)
        };
        let mut items: Vec<((usize, usize), StatusItem)> = std::mem::take(&mut self.status_items)
            .into_iter()
            .map(|item| (rank(&item, self), item))
            .collect();
        items.sort_by_key(|(rank, _)| *rank);
        self.status_items = items.into_iter().map(|(_, item)| item).collect();
        cx.notify();
    }

    /// A line in a plugin's log.
    fn log(&self, plugin: &str, level: Level, text: &str) {
        if let Some(state) = self.plugin(plugin) {
            state.log.write(level, text);
        }
    }

    /// Delivers an event to every running plugin.
    fn broadcast(&self, event: Event) {
        for instance in self.instances.values() {
            instance.send_event(event.clone());
        }
    }

    // --- Notifications ---

    fn with_center(
        &self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut NotificationCenter, &mut Context<NotificationCenter>),
    ) {
        if let Some(center) = self.center.as_ref().and_then(WeakEntity::upgrade) {
            center.update(cx, f);
        }
    }

    /// Posts a notification to the window's center.
    fn post(&self, notification: Notification, cx: &mut Context<Self>) -> Option<NotificationId> {
        let center = self.center.as_ref().and_then(WeakEntity::upgrade)?;
        Some(center.update(cx, |center, cx| center.notify(notification, cx)))
    }

    /// Tells the user a plugin stopped (or its build failed), with Restart, Disable and Details.
    fn report_stop(&mut self, id: &str, build: bool, cx: &mut Context<Self>) {
        let Some(state) = self.plugin(id) else {
            return;
        };
        let PluginStatus::Stopped { error, .. } = &state.status else {
            return;
        };
        let name = state.name().to_string();
        let title = if build {
            trf("Couldn't build plugin «{0}»", &[&name])
        } else {
            trf("Plugin «{0}» stopped", &[&name])
        };
        let plugin: SharedString = id.to_string().into();
        let notification = Notification::error(title)
            .body(error.clone())
            .group(NotificationGroup::Plugins)
            .action(tr("Restart"), RestartPlugin(plugin.clone()))
            .action(tr("Disable"), DisablePlugin(plugin.clone()))
            .action(tr("Details"), ShowStopDetails(plugin));
        if let Some(previous) = self.stop_notifications.remove(id) {
            self.with_center(cx, |center, cx| center.expire(previous, cx));
        }
        if let Some(posted) = self.post(notification, cx) {
            self.stop_notifications.insert(Arc::from(id), posted);
        }
    }

    // --- Questions ---

    fn has_pending_question(&self) -> bool {
        !self.pending_questions.is_empty()
    }
}

/// The plugin's settings: the manifest's defaults with the user's values over them.
fn settings_values(entry: &PluginEntry, cx: &App) -> Map<String, Value> {
    let mut values: Map<String, Value> = entry
        .manifest
        .settings
        .iter()
        .map(|setting| (setting.key.clone(), setting.default.clone()))
        .collect();
    for (key, value) in settings::plugin_values(entry.id(), cx) {
        if entry.manifest.setting(&key).is_some() {
            values.insert(key, value);
        }
    }
    values
}

fn undeclared(what: &str, id: &str) -> String {
    format!("The {what} \"{id}\" isn't declared in flux-plugin.toml")
}

pub(crate) fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// A plugin's notification as the window shows it: in the plugin's group, its actions run the
/// plugin's commands.
fn plugin_notification(plugin: &str, notification: &ApiNotification) -> Notification {
    let kind = match notification.kind {
        ApiKind::Info => NotificationKind::Info,
        ApiKind::Success => NotificationKind::Success,
        ApiKind::Warning => NotificationKind::Warning,
        ApiKind::Error => NotificationKind::Error,
    };
    let mut out = Notification::new(kind, notification.title.clone())
        .group(NotificationGroup::Plugin(plugin.to_string().into()));
    if let Some(body) = &notification.body {
        out = out.body(body.clone());
    }
    for action in &notification.actions {
        out = out.action(
            action.label.clone(),
            RunCommand {
                plugin: plugin.to_string().into(),
                command: action.command.clone().into(),
                origin: Some(CommandOrigin::new(CommandSource::Notification)),
            },
        );
    }
    if notification.sticky {
        out.sticky = true;
    }
    out
}

/// The content of a stopped plugin's tool window: why, and Restart.
/// The declarative files of a plugin under development with their times of change: its manifest
/// and the files it names (queries, grammars, themes, icon sets). A change reloads it.
fn declarative_files(entry: &PluginEntry) -> Vec<(String, Option<SystemTime>)> {
    let Some(dir) = entry.files.dir() else {
        return Vec::new();
    };
    std::iter::once(flux_plugin::registry::MANIFEST)
        .chain(entry.manifest.named_files())
        .map(|file| {
            let modified = std::fs::metadata(dir.join(file))
                .and_then(|meta| meta.modified())
                .ok();
            (file.to_string(), modified)
        })
        .collect()
}

fn stopped_view(name: &str, error: &str) -> View {
    let span = |text: String, tone: Tone, bold: bool| Span {
        text,
        tone,
        bold,
        code: false,
        highlight: false,
    };
    View {
        elements: vec![
            Element {
                id: "flux.stopped".into(),
                kind: ElementKind::Column,
                children: vec![1, 2, 3],
            },
            Element {
                id: "flux.stopped.title".into(),
                kind: ElementKind::Text(vec![span(
                    trf("Plugin «{0}» stopped", &[&name]),
                    Tone::Normal,
                    true,
                )]),
                children: Vec::new(),
            },
            Element {
                id: "flux.stopped.error".into(),
                kind: ElementKind::Text(vec![span(error.to_string(), Tone::Error, false)]),
                children: Vec::new(),
            },
            Element {
                id: RESTART_ELEMENT.into(),
                kind: ElementKind::Button(ButtonSpec {
                    label: Some(tr("Restart").to_string()),
                    icon: None,
                    tooltip: None,
                    primary: true,
                    enabled: true,
                }),
                children: Vec::new(),
            },
        ],
    }
}

/// The content of a tool window while its plugin is being built.
fn building_view() -> View {
    View {
        elements: vec![
            Element {
                id: "flux.building".into(),
                kind: ElementKind::Column,
                children: vec![1, 2],
            },
            Element {
                id: "flux.building.text".into(),
                kind: ElementKind::Text(vec![Span {
                    text: tr("Building the plugin…").to_string(),
                    tone: Tone::Muted,
                    bold: false,
                    code: false,
                    highlight: false,
                }]),
                children: Vec::new(),
            },
            Element {
                id: "flux.building.progress".into(),
                kind: ElementKind::Progress(None),
                children: Vec::new(),
            },
        ],
    }
}

// --- Keys ---

/// The plugins' keys looked at so far: bound, or found taken or invalid (gpui can't unbind keys:
/// a plugin's keys are bound once per launch, their handlers ignore a plugin that is off, and a
/// restart doesn't repeat the warnings).
#[derive(Default)]
struct BoundKeys(HashSet<String>);

impl Global for BoundKeys {}

/// Binds a plugin's keys to `action` in the window, unless the keystrokes are invalid or Flux
/// (or another plugin) already uses them; the plugin's log says why not.
fn bind_plugin_keys<A: Action + std::fmt::Debug>(
    keys: &str,
    action: A,
    what: &str,
    log: &PluginLog,
    cx: &mut App,
) {
    let marker = format!("{action:?} {keys}");
    if !cx.default_global::<BoundKeys>().0.insert(marker) {
        return;
    }
    let Some(strokes) = parse_keystrokes(keys) else {
        return log.write(
            Level::Warn,
            &format!("The keys \"{keys}\" of the {what} aren't valid"),
        );
    };
    let taken = cx
        .key_bindings()
        .borrow()
        .bindings()
        .find(|binding| same_keystrokes(binding.keystrokes(), &strokes))
        .map(|binding| binding.action().name().to_string());
    if let Some(name) = taken {
        return log.write(
            Level::Warn,
            &format!("The keys \"{keys}\" of the {what} are taken by {name}"),
        );
    }
    cx.bind_keys([KeyBinding::new(keys, action, Some("Workspace"))]);
}

/// `cmd-shift-t`, `cmd-k cmd-t`: the keystrokes, none if any of them is invalid.
fn parse_keystrokes(keys: &str) -> Option<Vec<Keystroke>> {
    let strokes = keys
        .split_whitespace()
        .map(Keystroke::parse)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    (!strokes.is_empty()).then_some(strokes)
}

/// The same keystrokes, modifiers and keys.
fn same_keystrokes(bound: &[KeybindingKeystroke], wanted: &[Keystroke]) -> bool {
    bound.len() == wanted.len()
        && bound.iter().zip(wanted).all(|(bound, wanted)| {
            bound.inner().modifiers == wanted.modifiers && bound.inner().key == wanted.key
        })
}

// --- The window's side ---

/// The plugins' actions on the window: their commands, the manager, the notifications about a
/// stopped plugin.
pub fn workspace_actions(root: Div, cx: &mut Context<Workspace>) -> Div {
    root.on_action(cx.listener(|this, action: &RunCommand, _, cx| {
        this.plugins.update(cx, |store, cx| {
            store.run_command(&action.plugin, &action.command, action.origin.as_ref(), cx)
        })
    }))
    .on_action(cx.listener(|this, _: &OpenManager, window, cx| {
        crate::plugin_manager::open(this, window, cx)
    }))
    .on_action(cx.listener(|this, _: &InstallFromDisk, window, cx| {
        crate::plugin_manager::install_from_disk(this, window, cx)
    }))
    .on_action(cx.listener(|this, _: &ReloadDevPlugins, _, cx| {
        this.plugins
            .update(cx, |store, cx| store.reload_dev_plugins(cx))
    }))
    .on_action(cx.listener(|this, action: &RestartPlugin, _, cx| {
        this.plugins
            .update(cx, |store, cx| store.restart(&action.0, cx))
    }))
    .on_action(cx.listener(|this, action: &DisablePlugin, _, cx| {
        this.plugins
            .update(cx, |store, cx| store.set_enabled(&action.0, false, cx))
    }))
    .on_action(cx.listener(|this, action: &ShowStopDetails, window, cx| {
        show_stop_details(this, &action.0, window, cx)
    }))
}

/// Why a plugin stopped, in a dialog with the whole story; Restart starts it again.
fn show_stop_details(
    workspace: &mut Workspace,
    id: &SharedString,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let store = workspace.plugins.read(cx);
    let Some(name) = store.plugin(id).map(|state| state.name().to_string()) else {
        return;
    };
    let Some((error, details)) = store.stop_reason(id) else {
        // It runs again: nothing to tell.
        return;
    };
    let mut dialog = Dialog::warning(trf("Plugin «{0}» stopped", &[&name]))
        .message(error)
        .primary(tr("Restart"))
        .cancel(tr("Close"));
    if !details.trim().is_empty() {
        dialog = dialog.details(details);
    }
    let answer = dialog.show(window, cx);
    let plugins = workspace.plugins.downgrade();
    let id = id.clone();
    cx.spawn(async move |_, cx| {
        if answer.await == Some(0) {
            plugins.update(cx, |store, cx| store.restart(&id, cx)).ok();
        }
    })
    .detach();
}

/// Carries out a plugin's call that needs the window: a question, a document.
pub(crate) fn handle_call(
    workspace: &mut Workspace,
    plugin: &str,
    call: &HostCall,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    match call {
        HostCall::Ask { id, question } => ask(workspace, plugin, *id, question, window, cx),
        HostCall::AskText { id, question } => {
            let pending = PendingQuestion {
                plugin: Arc::from(plugin),
                id: *id,
                question: question.clone(),
            };
            if workspace.modal_open() {
                // A dialog or a popup of the user's is open: the question waits for it.
                workspace
                    .plugins
                    .update(cx, |store, _| store.pending_questions.push_back(pending));
            } else {
                ask_text(workspace, pending, window, cx);
            }
        }
        HostCall::Editor { call, reply } => editor_call(workspace, plugin, call, reply, window, cx),
        HostCall::Window { call, reply } => {
            crate::plugin_calls::window_call(workspace, plugin, call, reply, window, cx)
        }
        // The store carries out the rest itself.
        _ => {}
    }
}

/// A plugin's question in a Flux dialog; the answer goes back as `dialog-answered`.
fn ask(
    workspace: &mut Workspace,
    plugin: &str,
    id: u64,
    question: &Question,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let level = match question.level {
        DialogLevel::Info => PromptLevel::Info,
        DialogLevel::Warning => PromptLevel::Warning,
        DialogLevel::Critical => PromptLevel::Critical,
    };
    let mut dialog = Dialog::new(level, question.title.clone());
    if let Some(message) = &question.message {
        dialog = dialog.message(message.clone());
    }
    if let Some(details) = &question.details {
        dialog = dialog.details(details.clone());
    }
    for button in &question.buttons {
        let role = match button.role {
            ApiButtonRole::Primary => ButtonRole::Primary,
            ApiButtonRole::Normal => ButtonRole::Normal,
            ApiButtonRole::Danger => ButtonRole::Danger,
            ApiButtonRole::Cancel => ButtonRole::Cancel,
        };
        dialog = dialog.button(button.label.clone(), role);
    }
    if question.buttons.is_empty() {
        dialog = dialog.primary(tr("OK"));
    }
    let answer = dialog.show(window, cx);
    let plugins = workspace.plugins.downgrade();
    let plugin = plugin.to_string();
    cx.spawn(async move |_, cx| {
        let answer = answer.await.map(|index| index as u32);
        plugins
            .update(cx, |store, _| {
                store.send_to(&plugin, Event::DialogAnswered((id, answer)))
            })
            .ok();
    })
    .detach();
}

/// A plugin's question with a text field; the answer goes back as `text-answered` (none when the
/// dialog is dismissed).
fn ask_text(
    workspace: &mut Workspace,
    pending: PendingQuestion,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let PendingQuestion {
        plugin,
        id,
        question,
    } = pending;
    let (sender, receiver) = oneshot::channel::<String>();
    workspace.toggle_dialog(window, cx, move |window, cx| {
        let placeholder = question.placeholder.clone().unwrap_or_default();
        let mut dialog = InputDialog::new(question.title.clone(), placeholder, window, cx)
            .text(&question.text, cx)
            .confirm_label(question.confirm.clone());
        if let Some(message) = &question.message {
            dialog = dialog.subtitle(message.clone());
        }
        // Dismissed, the dialog drops the sender: the answer is none.
        dialog.on_confirm(move |text, _, _, _| {
            sender.send(text).ok();
        })
    });
    let plugins = workspace.plugins.downgrade();
    cx.spawn(async move |_, cx| {
        let answer = receiver.await.ok();
        plugins
            .update(cx, |store, _| {
                store.send_to(&plugin, Event::TextAnswered((id, answer)))
            })
            .ok();
    })
    .detach();
}

/// The overlay window closed: a plugin's question that waited for it is asked now.
pub(crate) fn ask_pending(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if workspace.modal_open() || !workspace.plugins.read(cx).has_pending_question() {
        return;
    }
    let pending = workspace
        .plugins
        .update(cx, |store, _| store.pending_questions.pop_front());
    if let Some(pending) = pending {
        ask_text(workspace, pending, window, cx);
    }
}

/// A call of the `editors` interface: the reply goes back through `reply` (the plugin's thread
/// waits for it).
fn editor_call(
    workspace: &mut Workspace,
    plugin: &str,
    call: &EditorCall,
    reply: &Sender<EditorReply>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let send = |answer: EditorReply| {
        reply.send(answer).ok();
    };
    let find = |id: u64, cx: &App| {
        workspace
            .editors(cx)
            .into_iter()
            .find(|editor| editor.entity_id().as_u64() == id)
    };
    let closed = || Err(tr("The document is closed").to_string());
    match call {
        EditorCall::Active => send(EditorReply::Editor(
            workspace
                .active_editor()
                .map(|editor| editor_info(&editor, cx)),
        )),
        EditorCall::List => send(EditorReply::Editors(
            workspace
                .editors(cx)
                .iter()
                .map(|editor| editor_info(editor, cx))
                .collect(),
        )),
        EditorCall::Text(id) => send(EditorReply::Text(
            find(*id, cx).map(|editor| editor.read(cx).document.text().to_string()),
        )),
        EditorCall::Selections(id) => send(EditorReply::Selections(
            find(*id, cx)
                .map(|editor| selections_of(editor.read(cx)))
                .unwrap_or_default(),
        )),
        EditorCall::SetSelections(id, ranges) => {
            let Some(editor) = find(*id, cx) else {
                return send(EditorReply::Done(closed()));
            };
            if ranges.is_empty() {
                return send(EditorReply::Done(Err(tr("No selections given").into())));
            }
            editor.update(cx, |editor, cx| {
                let text = editor.document.text().clone();
                let ranges = ranges
                    .iter()
                    .map(|range| to_offset(&text, &range.start)..to_offset(&text, &range.end))
                    .collect();
                editor.select_ranges(ranges, 0, cx);
            });
            send(EditorReply::Done(Ok(())))
        }
        EditorCall::Edit(id, edits) => {
            let Some(editor) = find(*id, cx) else {
                return send(EditorReply::Done(closed()));
            };
            let result = editor.update(cx, |editor, cx| {
                if editor.read_only || editor.message.is_some() {
                    return Err(tr("The document is read-only").to_string());
                }
                let edits = edit_ranges(editor.document.text(), edits)?;
                editor.replace_ranges(edits, cx);
                Ok(())
            });
            send(EditorReply::Done(result))
        }
        EditorCall::Save(id) => {
            let Some(editor) = find(*id, cx) else {
                return send(EditorReply::Done(closed()));
            };
            editor.update(cx, |editor, cx| editor.save(cx).detach());
            send(EditorReply::Done(Ok(())))
        }
        EditorCall::Open(path, selection) => {
            // A click on a row of the plugin's tool window opened it: the keyboard stays there.
            let focus = !workspace
                .plugins
                .read(cx)
                .tool_windows()
                .iter()
                .any(|tool| tool.plugin.as_ref() == plugin && tool.view.read(cx).opened_by_click());
            open_for_plugin(
                workspace,
                path,
                *selection,
                focus,
                reply.clone(),
                window,
                cx,
            )
        }
    }
}

/// `editors.open`: opens a file (relative paths are in the project) or goes to its tab, selects
/// the span in the middle of the view, and replies with the document's id. `focus` — the editor
/// takes the keyboard; without it, focus stays where it was (a tool window that opens a file on a
/// single click keeps it, as the file tree does).
pub(crate) fn open_for_plugin(
    workspace: &mut Workspace,
    path: &str,
    selection: Option<ApiRange>,
    focus: bool,
    reply: Sender<EditorReply>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let path = resolve(workspace.root(), path);
    let guard = OpenReply {
        reply: Some(reply),
        path: path.clone(),
    };
    workspace.open_and(path, focus, window, cx, move |this, cx| {
        let Some(editor) = this.active_editor() else {
            return;
        };
        if let Some(selection) = selection {
            editor.update(cx, |editor, cx| {
                let text = editor.document.text().clone();
                let start = to_offset(&text, &selection.start);
                let end = to_offset(&text, &selection.end);
                editor.select_range(start..end, cx);
            });
        }
        guard.send(EditorReply::Opened(Ok(editor.entity_id().as_u64())));
    });
}

/// The reply to `editors.open`: the document's id once it is open; an error when the opening
/// fails (the callback that would reply is dropped).
struct OpenReply {
    reply: Option<Sender<EditorReply>>,
    path: PathBuf,
}

impl OpenReply {
    fn send(mut self, answer: EditorReply) {
        if let Some(reply) = self.reply.take() {
            reply.send(answer).ok();
        }
    }
}

impl Drop for OpenReply {
    fn drop(&mut self) {
        if let Some(reply) = self.reply.take() {
            let error = trf("Couldn't open {0}", &[&self.path.display()]);
            reply.send(EditorReply::Opened(Err(error))).ok();
        }
    }
}

/// A plugin's path: absolute, or relative to the project root.
pub(crate) fn resolve(root: Option<&Path>, path: &str) -> PathBuf {
    let path = Path::new(path);
    match root {
        Some(root) if path.is_relative() => root.join(path),
        _ => path.to_path_buf(),
    }
}

/// A document as plugins see it.
pub(crate) fn editor_info(editor: &Entity<Editor>, cx: &App) -> EditorInfo {
    let read = editor.read(cx);
    EditorInfo {
        id: editor.entity_id().as_u64(),
        path: read.document.path().map(path_text),
        language: read.status_info().language,
        modified: read.document.is_modified(),
    }
}

/// The selections of a document, the primary one first; a range keeps its direction (the
/// cursor is at `end`).
pub(crate) fn selections_of(editor: &Editor) -> Vec<ApiRange> {
    let text = editor.document.text();
    let selection = editor.document.selection();
    let primary = selection.primary_index();
    let mut ranges = vec![selection.primary()];
    ranges.extend(
        selection
            .ranges()
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != primary)
            .map(|(_, range)| *range),
    );
    ranges
        .into_iter()
        .map(|range| ApiRange {
            start: to_position(text, range.anchor),
            end: to_position(text, range.head),
        })
        .collect()
}

/// A position as an offset in characters; past the end of a line — the line's end, past the last
/// line — the last line.
pub(crate) fn to_offset(text: &Rope, position: &Position) -> usize {
    let line = (position.line as usize).min(text.len_lines().saturating_sub(1));
    line_start(text, line) + (position.column as usize).min(line_len(text, line))
}

pub(crate) fn to_position(text: &Rope, offset: usize) -> Position {
    let offset = offset.min(text.len_chars());
    let line = text.char_to_line(offset);
    Position {
        line: line as u32,
        column: (offset - line_start(text, line)) as u32,
    }
}

/// A plugin's edits as offset ranges in ascending order; overlapping edits are an error.
fn edit_ranges(
    text: &Rope,
    edits: &[TextEdit],
) -> Result<Vec<(std::ops::Range<usize>, String)>, String> {
    let mut ranges: Vec<(std::ops::Range<usize>, String)> = edits
        .iter()
        .map(|edit| {
            let a = to_offset(text, &edit.range.start);
            let b = to_offset(text, &edit.range.end);
            (a.min(b)..a.max(b), edit.text.clone())
        })
        .collect();
    ranges.sort_by_key(|(range, _)| (range.start, range.end));
    if ranges
        .windows(2)
        .any(|pair| pair[1].0.start < pair[0].0.end)
    {
        return Err(tr("The edits overlap").to_string());
    }
    Ok(ranges)
}

/// The plugins' status bar items: dim text, a tooltip, a click runs the item's command.
pub(crate) fn status_bar_items(workspace: &Workspace, cx: &App) -> Vec<AnyElement> {
    let ui = Theme::ui(cx);
    workspace
        .plugins
        .read(cx)
        .status_items()
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let command = item.command.clone();
            div()
                .id(("plugin-status", index))
                .flex_none()
                .h(px(20.))
                .px_1()
                .flex()
                .items_center()
                .rounded(px(RADIUS_SM))
                .whitespace_nowrap()
                .text_color(ui.dim)
                .when_some(item.tooltip.clone(), |item, tooltip| {
                    item.tooltip(ui::tooltip(tooltip, None))
                })
                .when_some(command, |item, command| {
                    item.cursor_pointer()
                        .hover(move |style| style.bg(ui.hover).text_color(ui.foreground))
                        .on_click(move |_, window, cx| {
                            window.dispatch_action(Box::new(command.clone()), cx)
                        })
                })
                .child(item.text.clone())
                .into_any_element()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn position(line: u32, column: u32) -> Position {
        Position { line, column }
    }

    fn edit(start: (u32, u32), end: (u32, u32), text: &str) -> TextEdit {
        TextEdit {
            range: ApiRange {
                start: position(start.0, start.1),
                end: position(end.0, end.1),
            },
            text: text.into(),
        }
    }

    #[test]
    fn positions_and_offsets() {
        let text = Rope::from_str("fn main() {\n    ok\n}");
        assert_eq!(to_offset(&text, &position(1, 4)), 16);
        // Past the end of a line: its end; past the last line: the last line.
        assert_eq!(to_offset(&text, &position(1, 99)), 18);
        assert_eq!(to_offset(&text, &position(9, 0)), 19);
        assert_eq!(to_position(&text, 16), position(1, 4));
        assert_eq!(to_position(&text, 999), position(2, 1));
        // Columns count characters, not bytes.
        let text = Rope::from_str("привет\nмир");
        assert_eq!(to_offset(&text, &position(1, 2)), 9);
        assert_eq!(to_position(&text, 9), position(1, 2));
    }

    #[test]
    fn edits_are_sorted_and_must_not_overlap() {
        let text = Rope::from_str("one two three");
        let ranges = edit_ranges(
            &text,
            &[edit((0, 8), (0, 13), "3"), edit((0, 4), (0, 0), "2")],
        )
        .unwrap();
        // A backward range is turned around; the edits go in text order.
        assert_eq!(ranges, vec![(0..4, "2".into()), (8..13, "3".into())]);
        assert!(edit_ranges(&text, &[edit((0, 0), (0, 5), ""), edit((0, 4), (0, 6), "")]).is_err());
        // Two insertions at one place are fine.
        assert!(
            edit_ranges(
                &text,
                &[edit((0, 3), (0, 3), "a"), edit((0, 3), (0, 3), "b")]
            )
            .is_ok()
        );
    }

    #[test]
    fn keystrokes_are_parsed_and_compared() {
        assert!(parse_keystrokes("cmd-shift-t").is_some());
        assert!(parse_keystrokes("cmd-k cmd-t").is_some_and(|strokes| strokes.len() == 2));
        assert!(parse_keystrokes("").is_none());
        let bound = KeyBinding::new("cmd-shift-t", OpenManager, None);
        let same = parse_keystrokes("cmd-shift-t").unwrap();
        let other = parse_keystrokes("cmd-t").unwrap();
        assert!(same_keystrokes(bound.keystrokes(), &same));
        assert!(!same_keystrokes(bound.keystrokes(), &other));
    }

    #[test]
    fn relative_paths_are_in_the_project() {
        let root = Path::new("/p");
        assert_eq!(
            resolve(Some(root), "src/a.rs"),
            PathBuf::from("/p/src/a.rs")
        );
        assert_eq!(resolve(Some(root), "/x/b.rs"), PathBuf::from("/x/b.rs"));
        assert_eq!(resolve(None, "c.rs"), PathBuf::from("c.rs"));
    }

    #[test]
    fn a_plugins_notification_runs_its_commands() {
        let notification = plugin_notification(
            "flux.todo",
            &ApiNotification {
                kind: ApiKind::Warning,
                title: "Ten TODOs".into(),
                body: Some("in 3 files".into()),
                actions: vec![flux_plugin::api::notifications::Action {
                    label: "Show".into(),
                    command: "show".into(),
                }],
                sticky: true,
            },
        );
        assert_eq!(notification.kind, NotificationKind::Warning);
        assert!(notification.sticky);
        assert_eq!(
            notification.group,
            NotificationGroup::Plugin("flux.todo".into())
        );
        let (label, action) = &notification.actions[0];
        assert_eq!(label.as_ref(), "Show");
        assert_eq!(
            action.as_any().downcast_ref::<RunCommand>(),
            Some(&RunCommand {
                plugin: "flux.todo".into(),
                command: "show".into(),
                origin: Some(CommandOrigin::new(CommandSource::Notification)),
            })
        );
    }
}
