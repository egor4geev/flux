//! Root view of the window: the file tree, tabs with documents and terminals, the terminal panel,
//! opening files, closing tabs and the window, and quitting, with prompts about unsaved changes and
//! running commands.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use flux_core::Document;
use flux_fs::remap;
use gpui::{
    Action, AnyElement, AnyView, App, AsyncApp, AsyncWindowContext, Bounds, ClickEvent, Context,
    DismissEvent, DragMoveEvent, Entity, EntityId, FocusHandle, Focusable, FontWeight, Global,
    KeyBinding, ManagedView, MouseButton, MouseDownEvent, MouseUpEvent, PathPromptOptions, Pixels,
    Point, Render, ScrollHandle, SharedString, Subscription, Task, WeakEntity, Window,
    WindowHandle, actions, anchored, deferred, div, prelude::*, px, relative,
};

use crate::commit_panel::{CommitPanel, CommitPanelEvent};
use crate::dialog::Dialog;
use crate::diff_view::{DiffSide, DiffView, DiffViewEvent};
use crate::editor::{self, Editor, EditorEvent};
use crate::file_tree::{self, FileTreeEvent, FileTreePanel};
use crate::find_bar::{self, FindBar};
use crate::git::{GitEvent, GitStore};
use crate::git_log::GitLogView;
use crate::git_window::{self, DraggedLogTab, GitWindow, GitWindowEvent};
use crate::i18n::{tr, trf, trn};
use crate::icons::{IconName, file_icon, icon};
use crate::launchpad::{self, Tool};
use crate::lsp::LspStore;
use crate::merge_view::{MergeView, MergeViewEvent};
use crate::notification_center::NotificationCenter;
use crate::notification_center::NotificationGroup;
use crate::notifications::{self, Notification};
use crate::notifications_panel::{self, NotificationsPanel};
use crate::plugins::{self, PluginStore, PluginStoreEvent, ToolKey};
use crate::project_search::{self, ProjectSearch, ProjectSearchEvent};
use crate::start_screen::{self, StartScreen};
use crate::terminal_group::{DraggedTerminal, TerminalGroup, TerminalGroupEvent};
use crate::terminal_panel::{self, TerminalPanel, TerminalPanelEvent};
use crate::terminal_view::TerminalLink;
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, GAP, RADIUS_MD, RADIUS_SM, STATUS_BAR_HEIGHT, TITLE_BAR_HEIGHT};
use crate::{command_palette, file_finder, go_to_line, recent};

/// The tab strip inside the editor island, and the tabs themselves.
const TAB_BAR_HEIGHT: f32 = 40.;
const TAB_HEIGHT: f32 = 30.;
/// Long file and directory names on a tab are shortened in the middle.
const TAB_LABEL_MAX_CHARS: usize = 32;
/// Offset of the overlay window (palette, file search) from the top of the window.
const MODAL_TOP: f32 = TITLE_BAR_HEIGHT + 32.;
/// Space for the macOS traffic lights in the title bar (the buttons are placed by `main` via
/// `traffic_light_position`).
const TRAFFIC_LIGHTS_WIDTH: f32 = 84.;
/// Project search is an overlay window: its share of the window width, and the upper limit.
const SEARCH_WIDTH: f32 = 0.84;
const SEARCH_MAX_WIDTH: f32 = 1080.;
/// In a window narrower than this, the file search bar is not shown in the title bar: it would not
/// fit between the buttons.
const TITLE_SEARCH_MIN_WINDOW: f32 = 920.;
/// Actions that only make sense while an operation is in progress or something conflicts: the
/// notifications offering them close when that is over.
const OPERATION_ACTIONS: &[&str] = &[
    "git::ResolveConflicts",
    "git::ContinueOperation",
    "git::AbortOperation",
    "git::SkipCommit",
    "git::ContinueRepoOperation",
    "git::AbortRepoOperation",
    "git::SkipRepoCommit",
];
/// How long a status bar message stays while a terminal tab is active (an editor keeps its own until
/// the next edit).
const STATUS_MESSAGE_DURATION: Duration = Duration::from_secs(5);

actions!(
    workspace,
    [
        Open,
        NewFile,
        CloseTab,
        CloseWindow,
        Quit,
        NextTab,
        PrevTab,
        LastTab,
    ]
);

/// Go to the tab with the given number (zero-based): ctrl-1…ctrl-8.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = workspace, no_json)]
pub struct ActivateTab(pub usize);

/// Make the directory the window's project root (a recent project on the start screen).
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = workspace, no_json)]
pub struct OpenProject(pub PathBuf);

pub fn init(cx: &mut App) {
    bind_keys(cx);
    cx.on_action(quit);
}

fn bind_keys(cx: &mut App) {
    let context = Some("Workspace");
    cx.bind_keys([
        KeyBinding::new("cmd-o", Open, context),
        KeyBinding::new("cmd-n", NewFile, context),
        KeyBinding::new("cmd-w", CloseTab, context),
        KeyBinding::new("cmd-shift-w", CloseWindow, context),
        KeyBinding::new("cmd-q", Quit, context),
        // macOS sends cmd-shift-] as cmd-}: shift is already accounted for in the character.
        KeyBinding::new("cmd-}", NextTab, context),
        KeyBinding::new("cmd-{", PrevTab, context),
        KeyBinding::new("cmd-shift-]", NextTab, context),
        KeyBinding::new("cmd-shift-[", PrevTab, context),
        KeyBinding::new("ctrl-tab", NextTab, context),
        KeyBinding::new("ctrl-shift-tab", PrevTab, context),
        KeyBinding::new("ctrl-9", LastTab, context),
        // When focus is outside the editor (file tree, search fields), the active document is
        // saved.
        KeyBinding::new("cmd-s", editor::Save, context),
    ]);
    // cmd-1 belongs to the file tree (as in JetBrains IDEs), so tabs by number are on ctrl.
    cx.bind_keys(
        (1..=8).map(|n| KeyBinding::new(&format!("ctrl-{n}"), ActivateTab(n - 1), context)),
    );
}

/// A location in a file to jump to: a zero-based line and columns in characters; `start..end` is
/// selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub path: PathBuf,
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

/// Root view of the window: the tabs (one editor per document, terminal tabs moved from the panel)
/// and the active one among them, the search panels, the terminal panel, the overlay windows.
pub struct Workspace {
    /// Project root: files (cmd-p) and text (cmd-shift-f) are searched within it.
    root: Option<PathBuf>,
    tabs: Vec<Tab>,
    active: usize,
    /// Focus handle of the empty window: without it, cmd-o, cmd-n, cmd-w, and cmd-q would not work.
    focus_handle: FocusHandle,
    tab_scroll: ScrollHandle,
    /// The start screen was in the last drawn frame (see `render_start`).
    start_drawn: bool,
    /// Unsaved documents are being reviewed: new close requests are ignored.
    closing: bool,
    /// How many batches of files are still being read in the background.
    loading: usize,
    /// Message in the empty window; when tabs are open, messages go to the editor's status bar.
    notice: Option<SharedString>,
    /// A status bar message while a terminal tab is active; it goes away after a few seconds.
    status_message: Option<SharedString>,
    status_message_task: Option<Task<()>>,
    /// Where a tab dragged over the tab strip would land: the strip shows a marker there.
    tab_drop: Option<usize>,
    /// The window title and the "has unsaved changes" flag as last set.
    title: String,
    edited: bool,
    /// Overlay window above the tabs: command palette, file search.
    modal: Option<Modal>,
    /// Back / Forward along go-to-definition jumps, and the language server request in flight.
    pub(crate) navigation: crate::navigation::NavState,
    /// The in-document find bar (between the tabs and the text).
    find_bar: Entity<FindBar>,
    /// Project search panel (below the text).
    project_search: Entity<ProjectSearch>,
    /// The file tree on the left; present only when there is a project root.
    file_tree: Option<TreePanel>,
    /// The tree is shown (cmd-1).
    tree_open: bool,
    /// The file last shown in the tree: the tree follows the active tab.
    revealed: Option<PathBuf>,
    /// Git of the project: repositories, changes, the branch in the title bar; recreated with the
    /// root.
    git: Entity<GitStore>,
    _git_subscription: Subscription,
    /// The width of the left island, shared by the tree and the commit window.
    left_width: ui::LeftIslandWidth,
    /// The commit window: in the left island instead of the tree (⌘0, ⌘K); made when first shown.
    commit_panel: Option<CommitPanelHandle>,
    commit_open: bool,
    /// The tree was shown when the commit window took its place: hiding the commit window brings it
    /// back.
    tree_before_commit: bool,
    /// Recent projects for the start screen, most recent first.
    recent: Vec<PathBuf>,
    /// Language servers of the project; recreated with the root.
    pub(crate) lsp: Entity<LspStore>,
    /// Terminals: an island under the editor (⌥F12).
    terminal_panel: Entity<TerminalPanel>,
    terminal_open: bool,
    /// The island under the editor shows the terminals or the Git window; they share its height.
    bottom_height: ui::BottomIslandHeight,
    /// The Git window (⌘9): the log and histories; made when first shown, recreated with the git
    /// hub.
    git_window: Option<GitWindowHandle>,
    git_open: bool,
    /// The notification center: the journal (the Notifications window) and the cards in the bottom
    /// right corner.
    notification_center: Entity<NotificationCenter>,
    /// The island on the right: the tool window shown in it, if any.
    right_tool: Option<RightTool>,
    /// The Notifications window (the journal of the notification center).
    notifications_panel: Entity<NotificationsPanel>,
    /// The window's plugins: their commands, tool windows (in the island on the right), status
    /// bar items; they learn what happens in the window.
    pub(crate) plugins: Entity<PluginStore>,
    _subscriptions: Vec<Subscription>,
}

/// A tool window of the island on the right: Notifications, or a plugin's window (the Claude window
/// of stage 9 takes the same slot).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RightTool {
    Notifications,
    Plugin(ToolKey),
}

/// The commit window and the subscription to its events; recreated with the git hub.
struct CommitPanelHandle {
    panel: Entity<CommitPanel>,
    _subscription: Subscription,
}

/// The Git window and the subscriptions to it.
struct GitWindowHandle {
    window: Entity<GitWindow>,
    _subscriptions: [Subscription; 2],
}

/// The file tree and the subscription to its events; recreated whenever the project root changes.
struct TreePanel {
    panel: Entity<FileTreePanel>,
    _subscription: Subscription,
}

/// A tab of the editor area.
struct Tab {
    item: TabItem,
    /// The tab strip and the window title follow the item: an editor's changes, a terminal tab's
    /// events.
    _subscriptions: Vec<Subscription>,
}

/// What a tab shows: a document, a terminal tab brought over from the panel, a diff, or the merge
/// tool of a conflicted file.
#[derive(Clone, PartialEq)]
enum TabItem {
    Editor(Entity<Editor>),
    Terminal(Entity<TerminalGroup>),
    Diff(Entity<DiffView>),
    Merge(Entity<MergeView>),
    /// A tab of the Git window (the log, a history) brought over from it.
    Log(Entity<GitLogView>),
}

impl TabItem {
    fn editor(&self) -> Option<&Entity<Editor>> {
        match self {
            TabItem::Editor(editor) => Some(editor),
            _ => None,
        }
    }

    fn terminal(&self) -> Option<&Entity<TerminalGroup>> {
        match self {
            TabItem::Terminal(group) => Some(group),
            _ => None,
        }
    }

    fn diff(&self) -> Option<&Entity<DiffView>> {
        match self {
            TabItem::Diff(view) => Some(view),
            _ => None,
        }
    }

    fn merge(&self) -> Option<&Entity<MergeView>> {
        match self {
            TabItem::Merge(view) => Some(view),
            _ => None,
        }
    }

    fn log(&self) -> Option<&Entity<GitLogView>> {
        match self {
            TabItem::Log(view) => Some(view),
            _ => None,
        }
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self {
            TabItem::Log(view) => view.focus_handle(cx),
            TabItem::Editor(editor) => editor.focus_handle(cx),
            TabItem::Terminal(group) => group.focus_handle(cx),
            TabItem::Diff(view) => view.focus_handle(cx),
            TabItem::Merge(view) => view.focus_handle(cx),
        }
    }

    /// Unsaved changes: a document's, or those of the working copy a diff opened without a tab.
    fn is_modified(&self, cx: &App) -> bool {
        match self {
            TabItem::Editor(editor) => is_modified(editor, cx),
            TabItem::Diff(view) => owned_editor(view, cx).is_some_and(|e| is_modified(&e, cx)),
            TabItem::Merge(view) => view.read(cx).is_modified(cx),
            TabItem::Terminal(_) | TabItem::Log(_) => false,
        }
    }
}

/// A file tab being dragged along the tab strip: it is also the label next to the pointer.
#[derive(Clone)]
struct DraggedEditorTab {
    editor: Entity<Editor>,
    name: SharedString,
}

/// Overlay window. It closes by itself (`DismissEvent`: Esc, making a choice) or when the same
/// window is invoked again; focus returns to where it was before opening. How else it goes depends
/// on its [`ModalKind`].
struct Modal {
    view: AnyView,
    kind: ModalKind,
    focus_handle: FocusHandle,
    previous_focus: Option<FocusHandle>,
    /// Shown at this window point (rename, under its symbol) instead of the top center.
    anchor: Option<Point<Pixels>>,
    _subscriptions: Vec<Subscription>,
}

/// The two kinds of overlay windows (wiki: "Design System", popups).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModalKind {
    /// A light popup — a search list (palette, file search), a quick list or a menu (⌃V, branches,
    /// usages), an inline field (rename): a click outside closes it, and so does focus leaving it
    /// (for example, ⌘N opened a tab).
    Popup,
    /// A dialog with data entered or chosen (Push, New Branch, Settings): modal, as in JetBrains
    /// IDEs. The window behind it doesn't take clicks, a click outside doesn't close it, and
    /// shortcuts don't replace it with a popup; focus leaving it (a question on top of it) doesn't
    /// close it either.
    Dialog,
}

/// Where the paths came from: this determines where errors are reported.
#[derive(Clone, Copy)]
enum Source {
    CommandLine,
    Dialog,
}

/// A file that could not be opened.
struct OpenError {
    path: PathBuf,
    reason: String,
}

impl Workspace {
    /// A window for the project `root` with the files from the command line (read in the
    /// background). Without files, the start screen is shown.
    pub fn new(
        root: Option<PathBuf>,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let this = cx.weak_entity();
        // The window's red button: the same as cmd-shift-w.
        window.on_window_should_close(cx, move |window, cx| {
            this.update(cx, |this, cx| this.request_close_window(window, cx))
                .unwrap_or(true)
        });
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle);
        let find_bar = cx.new(|cx| FindBar::new(window, cx));
        let project_search = cx.new(|cx| ProjectSearch::new(root.clone(), window, cx));
        let lsp = Self::build_lsp(root.clone(), cx);
        let (git, git_subscription) = Self::build_git(root.clone(), window, cx);
        let left_width = ui::LeftIslandWidth::new();
        let file_tree = root.clone().map(|root| {
            let tree = Self::build_tree(root, left_width.clone(), window, cx);
            tree.panel
                .update(cx, |panel, cx| panel.set_git(git.clone(), cx));
            tree
        });
        let bottom_height = ui::BottomIslandHeight::new();
        let terminal_panel =
            cx.new(|cx| TerminalPanel::new(root.clone(), bottom_height.clone(), window, cx));
        let notification_center = cx.new(NotificationCenter::new);
        // The width of the right island: its tool windows share one cell (`ui::RightIslandWidth`).
        let right_width = ui::RightIslandWidth::new();
        let notifications_panel = cx.new(|cx| {
            NotificationsPanel::new(notification_center.clone(), right_width.clone(), cx)
        });
        let plugins = cx.new(|cx| PluginStore::new(root.clone(), right_width, cx));
        plugins.update(cx, |store, _| {
            store.set_notification_center(notification_center.downgrade())
        });
        // The panels open and close on their own (Esc, ×); when they do, the window layout changes
        // too.
        let subscriptions = vec![
            // Files may have changed while the window was inactive (git in a terminal, another
            // editor): the watcher reports them, and a refresh on return makes sure.
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.git.update(cx, |git, cx| git.refresh(cx));
                    this.sync_documents(None, cx);
                }
            }),
            cx.observe(&find_bar, |_, _, cx| cx.notify()),
            cx.observe(&project_search, |_, _, cx| cx.notify()),
            cx.observe(&terminal_panel, |_, _, cx| cx.notify()),
            cx.observe(&notification_center, |_, _, cx| cx.notify()),
            cx.observe(&notifications_panel, |_, _, cx| cx.notify()),
            cx.observe(&plugins, |_, _, cx| cx.notify()),
            cx.subscribe_in(&plugins, window, Self::on_plugin_store_event),
            cx.subscribe_in(&terminal_panel, window, Self::on_terminal_panel_event),
            cx.subscribe_in(
                &project_search,
                window,
                |this, _, event, window, cx| match event {
                    ProjectSearchEvent::Open { location, focus } => {
                        this.open_location(location.clone(), *focus, window, cx)
                    }
                    ProjectSearchEvent::FocusEditor => this.focus_active(window, cx),
                },
            ),
        ];
        let recent = match &root {
            Some(root) => recent::record(root),
            None => recent::load(),
        };
        let mut workspace = Self {
            root,
            tabs: Vec::new(),
            active: 0,
            focus_handle,
            tab_scroll: ScrollHandle::new(),
            start_drawn: false,
            closing: false,
            loading: 0,
            notice: None,
            status_message: None,
            status_message_task: None,
            tab_drop: None,
            title: String::new(),
            edited: false,
            modal: None,
            navigation: Default::default(),
            find_bar,
            project_search,
            file_tree,
            tree_open: true,
            revealed: None,
            git,
            _git_subscription: git_subscription,
            left_width,
            commit_panel: None,
            commit_open: false,
            tree_before_commit: false,
            recent,
            lsp,
            terminal_panel,
            terminal_open: false,
            bottom_height,
            git_window: None,
            git_open: false,
            notification_center,
            right_tool: None,
            notifications_panel,
            plugins,
            _subscriptions: subscriptions,
        };
        if !paths.is_empty() {
            workspace.open_paths(paths, Source::CommandLine, window, cx);
        }
        workspace
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// A new project root (cmd-o with a directory).
    fn set_root(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let root = fs::canonicalize(&root).unwrap_or(root);
        self.project_search
            .update(cx, |search, cx| search.set_root(Some(root.clone()), cx));
        let (git, git_subscription) = Self::build_git(Some(root.clone()), window, cx);
        self.git = git;
        self._git_subscription = git_subscription;
        self.commit_panel = None;
        self.commit_open = false;
        // The Git window and its tabs in the editor area belong to the old hub.
        self.git_window = None;
        self.git_open = false;
        while let Some(index) = self.tabs.iter().position(|tab| tab.item.log().is_some()) {
            self.remove_tab_at(index, window, cx);
        }
        let tree = Self::build_tree(root.clone(), self.left_width.clone(), window, cx);
        tree.panel
            .update(cx, |panel, cx| panel.set_git(self.git.clone(), cx));
        self.file_tree = Some(tree);
        self.tree_open = true;
        self.revealed = None;
        self.recent = recent::record(&root);
        self.show_message(trf("Project: {0}", &[&tilde(&root)]).into(), cx);
        self.root = Some(root.clone());
        self.terminal_panel
            .update(cx, |panel, _| panel.set_root(Some(root.clone())));
        self.lsp = Self::build_lsp(Some(root.clone()), cx);
        self.plugins
            .update(cx, |store, cx| store.set_root(Some(root), cx));
        for editor in self.editors(cx) {
            self.lsp.update(cx, |store, cx| store.register(&editor, cx));
            self.git.update(cx, |store, cx| store.register(&editor, cx));
        }
        self.reveal_active(cx);
        cx.notify();
    }

    /// Language servers for a project root; the status bar follows their status, notifications
    /// tell what happened to them.
    fn build_lsp(root: Option<PathBuf>, cx: &mut Context<Self>) -> Entity<LspStore> {
        let lsp = cx.new(|cx| LspStore::new(root, cx));
        cx.observe(&lsp, |_, _, cx| cx.notify()).detach();
        cx.subscribe(&lsp, |this, _, event, cx| match event {
            crate::lsp::LspEvent::Notify(notification) => this.notify(notification.clone(), cx),
        })
        .detach();
        lsp
    }

    /// Git of a project root; its hints go to the status bar, what happened — to notifications.
    fn build_git(
        root: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<GitStore>, Subscription) {
        let git = cx.new(|cx| GitStore::new(root, cx));
        cx.observe(&git, |this, git, cx| {
            // A card that offers the next step of a merge or a rebase ("Resolve…", "Continue
            // Rebase", "Abort") is stale once nothing is in progress and nothing conflicts.
            let idle = {
                let git = git.read(cx);
                git.repos_in_progress().is_empty() && git.conflicts().is_empty()
            };
            if idle {
                this.notification_center.update(cx, |center, cx| {
                    center.expire_where(|card| card.offers(OPERATION_ACTIONS), cx)
                });
            }
            cx.notify()
        })
        .detach();
        let subscription = cx.subscribe_in(&git, window, |this, _, event, window, cx| {
            this.on_git_event(event, window, cx)
        });
        (git, subscription)
    }

    pub(crate) fn git(&self) -> &Entity<GitStore> {
        &self.git
    }

    fn on_git_event(&mut self, event: &GitEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            GitEvent::Message(message) => self.show_message(message.clone(), cx),
            GitEvent::Notify(notification) => self.notify(notification.clone(), cx),
            GitEvent::WorkTreeChanged(repo) => self.work_tree_changed(*repo, window, cx),
        }
    }

    /// Closes the notifications that offer one of the actions named (the user did it another way).
    pub(crate) fn dismiss_notifications(&mut self, actions: &[&str], cx: &mut Context<Self>) {
        self.notification_center.update(cx, |center, cx| {
            center.expire_where(|card| card.offers(actions), cx)
        });
    }

    /// A notification: to the journal and, as its group's display says, a card in the corner.
    pub(crate) fn notify(
        &mut self,
        notification: crate::notifications::Notification,
        cx: &mut Context<Self>,
    ) {
        self.notification_center
            .update(cx, |center, cx| center.notify(notification, cx));
    }

    /// A git operation changed files of a repository (checkout, merge, stash…): open documents
    /// take what is on disk now; unmodified ones whose files are gone close (a modified one stays,
    /// saving brings the file back).
    fn work_tree_changed(&mut self, repo: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_documents(None, cx);
        let Some(work_dir) = self
            .git
            .read(cx)
            .repos()
            .get(repo)
            .map(|entry| entry.repo.work_dir.clone())
        else {
            return;
        };
        let gone: Vec<PathBuf> = self
            .editors(cx)
            .iter()
            .filter_map(|editor| editor.read(cx).document.path().map(Path::to_path_buf))
            .filter(|path| path.starts_with(&work_dir) && !path.exists())
            .collect();
        if !gone.is_empty() {
            self.documents_removed(&gone, window, cx);
        }
    }

    /// The documents of the window: in tabs, and the working copies diffs opened without a tab.
    pub(crate) fn editors(&self, cx: &App) -> Vec<Entity<Editor>> {
        let mut editors: Vec<Entity<Editor>> = Vec::new();
        for tab in &self.tabs {
            let editor = match &tab.item {
                TabItem::Editor(editor) => Some(editor.clone()),
                TabItem::Diff(view) => owned_editor(view, cx),
                TabItem::Terminal(_) | TabItem::Merge(_) | TabItem::Log(_) => None,
            };
            if let Some(editor) = editor
                && !editors.contains(&editor)
            {
                editors.push(editor);
            }
        }
        editors
    }

    /// The file of the active tab: a document's, or the file a diff shows.
    pub(crate) fn active_path(&self, cx: &App) -> Option<PathBuf> {
        match self.active_item()? {
            TabItem::Editor(editor) => editor.read(cx).document.path().map(Path::to_path_buf),
            TabItem::Diff(view) => Some(view.read(cx).path().to_path_buf()),
            TabItem::Merge(view) => Some(view.read(cx).path().to_path_buf()),
            TabItem::Terminal(_) | TabItem::Log(_) => None,
        }
    }

    /// The active tab, if it is a document.
    pub(crate) fn active_editor(&self) -> Option<Entity<Editor>> {
        self.active_item().and_then(|item| item.editor().cloned())
    }

    /// The active tab, if it is a terminal tab.
    fn active_terminal(&self) -> Option<Entity<TerminalGroup>> {
        self.active_item().and_then(|item| item.terminal().cloned())
    }

    fn active_item(&self) -> Option<TabItem> {
        self.tabs.get(self.active).map(|tab| tab.item.clone())
    }

    fn index_of(&self, editor: &Entity<Editor>) -> Option<usize> {
        self.tabs
            .iter()
            .position(|tab| tab.item.editor() == Some(editor))
    }

    fn index_of_terminal(&self, group: &Entity<TerminalGroup>) -> Option<usize> {
        self.tabs
            .iter()
            .position(|tab| tab.item.terminal() == Some(group))
    }

    // --- Tabs ---

    /// Makes the tab active and moves focus into it (its editor, or its terminal).
    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let item = tab.item.clone();
        window.focus(&item.focus_handle(cx));
        self.active = index;
        self.tab_scroll.scroll_to_item(index);
        self.status_message = None;
        if let TabItem::Terminal(group) = &item {
            group.update(cx, |group, cx| group.clear_bell(cx));
        }
        // A terminal tab has no find bar: it searches its output itself.
        let editor = item.editor().cloned();
        self.plugins
            .update(cx, |store, cx| store.set_active_editor(editor.as_ref(), cx));
        self.find_bar
            .update(cx, |bar, cx| bar.set_active_editor(editor, window, cx));
        self.reveal_active(cx);
        cx.notify();
    }

    /// Focuses the active tab, or the window itself when there are no tabs.
    fn focus_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.active_item() {
            Some(item) => window.focus(&item.focus_handle(cx)),
            None => window.focus(&self.focus_handle),
        }
    }

    /// gpui scrolls the strip to a tab only if the strip has already been drawn; the request is
    /// lost in its first frame (launching with many files). In that case we repeat it before the
    /// next frame.
    fn retry_tab_scroll(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab_scroll.bounds().size.width > px(0.) {
            return;
        }
        cx.on_next_frame(window, |this, _, cx| {
            this.tab_scroll.scroll_to_item(this.active);
            cx.notify();
        });
    }

    fn activate_editor(
        &mut self,
        editor: &Entity<Editor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self.index_of(editor) {
            self.activate(index, window, cx);
        }
    }

    /// The adjacent tab, wrapping around: `step` = 1 is the next one, -1 the previous one.
    fn cycle(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let len = self.tabs.len() as isize;
        if len > 0 {
            let index = (self.active as isize + step).rem_euclid(len);
            self.activate(index as usize, window, cx);
        }
    }

    /// Adds a new tab to the right of the active one; it becomes the active tab.
    fn add_document(&mut self, document: Document, window: &mut Window, cx: &mut Context<Self>) {
        let editor = self.new_editor(document, window, cx);
        self.add_editor_tab(editor, window, cx);
    }

    /// An editor for a document, on the window's language servers and git.
    fn new_editor(
        &mut self,
        document: Document,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<Editor> {
        let editor = cx.new(|cx| Editor::new(document, window, cx));
        self.lsp.update(cx, |store, cx| store.register(&editor, cx));
        self.git.update(cx, |store, cx| store.register(&editor, cx));
        self.plugins
            .update(cx, |store, cx| store.register(&editor, cx));
        editor
    }

    /// A tab for an editor (a new one, or the working copy a diff opened without a tab).
    fn add_editor_tab(
        &mut self,
        editor: Entity<Editor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A document's path changes on "Save As": the tree then shows the new file.
        let observer = cx.observe(&editor, |this, _, cx| {
            this.reveal_active(cx);
            cx.notify()
        });
        let events = cx.subscribe(&editor, |this, editor, event: &EditorEvent, cx| {
            if let EditorEvent::SaveFailed(reason) = event {
                let name = editor.read(cx).document.display_name();
                let notification = Notification::error(trf("Couldn't save {0}", &[&name]))
                    .body(reason.clone())
                    .group(NotificationGroup::Files);
                this.notify(notification, cx)
            }
        });
        self.insert_tab(TabItem::Editor(editor), vec![observer, events], None, window, cx);
    }

    /// Adds a terminal tab (moved from the panel) at `index`, by default to the right of the
    /// active tab; it becomes the active tab.
    fn add_terminal_tab(
        &mut self,
        group: Entity<TerminalGroup>,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let subscriptions = vec![
            cx.subscribe_in(&group, window, Self::on_terminal_tab_event),
            cx.observe(&group, |_, _, cx| cx.notify()),
        ];
        self.insert_tab(TabItem::Terminal(group), subscriptions, index, window, cx);
    }

    fn insert_tab(
        &mut self,
        item: TabItem,
        subscriptions: Vec<Subscription>,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let index = index
            .unwrap_or(if self.tabs.is_empty() {
                0
            } else {
                self.active + 1
            })
            .min(self.tabs.len());
        self.tabs.insert(
            index,
            Tab {
                item,
                _subscriptions: subscriptions,
            },
        );
        self.notice = None;
        self.activate(index, window, cx);
    }

    /// Removes a document's tab.
    fn remove_tab(&mut self, editor: &Entity<Editor>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.index_of(editor) {
            self.remove_tab_at(index, window, cx);
        }
    }

    /// Removes the tab at `index` (its item is dropped unless someone else holds it: a terminal tab
    /// moving to the panel). If it was active, focus moves to a neighboring one (the right one; for
    /// the last tab, the left one); with no tabs left, to the window itself.
    fn remove_tab_at(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.active = 0;
            window.focus(&self.focus_handle);
            self.plugins
                .update(cx, |store, cx| store.set_active_editor(None, cx));
            self.find_bar
                .update(cx, |bar, cx| bar.set_active_editor(None, window, cx));
            return cx.notify();
        }
        let active = if index < self.active {
            self.active - 1
        } else {
            self.active.min(self.tabs.len() - 1)
        };
        self.activate(active, window, cx);
    }

    // --- Opening ---

    /// cmd-o: files open in tabs; a chosen directory becomes the project root.
    fn open(&mut self, _: &Open, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: true,
            multiple: true,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                let (dirs, files): (Vec<_>, Vec<_>) = paths.into_iter().partition(|p| p.is_dir());
                if let Some(dir) = dirs.into_iter().last() {
                    this.set_root(dir, window, cx);
                }
                this.open_paths(files, Source::Dialog, window, cx)
            })
            .ok();
        })
        .detach();
    }

    /// Opens files (as from cmd-o): those already open are just activated.
    pub fn open_files(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        self.open_paths(paths, Source::Dialog, window, cx);
    }

    /// Opens a file (or activates its tab) and selects a location in it. With `focus == false`,
    /// focus stays where it was (for example, in the search results panel).
    pub fn open_location(
        &mut self,
        location: Location,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = location.path.clone();
        self.open_and(path, focus, window, cx, move |this, cx| {
            this.select_location(&location, cx)
        });
    }

    /// Opens a file (or activates its tab). With `focus == false`, focus stays where it was (in the
    /// file tree).
    pub fn open_file(
        &mut self,
        path: PathBuf,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_and(path, focus, window, cx, |_, _| {});
    }

    /// Opens or activates a file, then calls `then` with it in the active tab. Without `focus`,
    /// focus returns to where it was before opening.
    pub(crate) fn open_and(
        &mut self,
        path: PathBuf,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
    ) {
        let keep_focus = window.focused(cx).filter(|_| !focus);
        if self.activate_path(&path, window, cx) {
            then(self, cx);
            return restore_focus(keep_focus, window);
        }
        self.loading += 1;
        let read = cx.background_executor().spawn({
            let path = path.clone();
            async move { read_document(path) }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = read.await;
            this.update_in(cx, |this, window, cx| {
                this.loading -= 1;
                this.finish_open(vec![result], Source::Dialog, window, cx);
                if this.activate_path(&path, window, cx) {
                    then(this, cx);
                    restore_focus(keep_focus, window);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Selects a location in the active editor.
    fn select_location(&mut self, location: &Location, cx: &mut Context<Self>) {
        if let Some(editor) = self.active_editor() {
            editor.update(cx, |editor, cx| {
                let start = editor.position(location.line, location.start);
                let end = editor.position(location.line, location.end);
                editor.select_range(start..end, cx);
            });
        }
    }

    /// Opens files in tabs, in order. Those already open are just activated; the rest are read in
    /// the background so that a large file does not block the window.
    fn open_paths(
        &mut self,
        paths: Vec<PathBuf>,
        source: Source,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let paths: Vec<PathBuf> = paths
            .into_iter()
            .filter(|path| !self.activate_path(path, window, cx))
            .collect();
        if paths.is_empty() {
            return self.finish_open(Vec::new(), source, window, cx);
        }
        self.loading += 1;
        let read = cx
            .background_executor()
            .spawn(async move { paths.into_iter().map(read_document).collect::<Vec<_>>() });
        cx.spawn_in(window, async move |this, cx| {
            let results = read.await;
            this.update_in(cx, |this, window, cx| {
                this.loading -= 1;
                this.finish_open(results, source, window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// If the file is already open (compared by canonical paths), activates its tab.
    fn activate_path(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let target = canonical(path);
        let found = self.tabs.iter().position(|tab| {
            tab.item.editor().is_some_and(|editor| {
                let document = &editor.read(cx).document;
                document
                    .path()
                    .is_some_and(|path| canonical(path) == target)
            })
        });
        if let Some(index) = found {
            self.activate(index, window, cx);
            return true;
        }
        // The working copy of a diff, opened without a tab: it gets its own tab, the same editor.
        let owned = self.tabs.iter().find_map(|tab| {
            let view = tab.item.diff()?;
            let editor = owned_editor(view, cx)?;
            let path = editor.read(cx).document.path().map(canonical)?;
            (path == target).then(|| (view.clone(), editor))
        });
        if let Some((view, editor)) = owned {
            view.update(cx, |view, _| view.set_owns_working(false));
            self.add_editor_tab(editor, window, cx);
            return true;
        }
        false
    }

    fn finish_open(
        &mut self,
        results: Vec<Result<Document, OpenError>>,
        source: Source,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut errors = Vec::new();
        for result in results {
            match result {
                Ok(document) => self.add_or_activate(document, window, cx),
                Err(error) => errors.push(error),
            }
        }
        match source {
            Source::CommandLine => self.report_startup(errors),
            Source::Dialog => self.report_dialog(errors, cx),
        }
    }

    /// The file may have been chosen twice while it was being read; in that case we activate the
    /// tab that is already open.
    fn add_or_activate(&mut self, document: Document, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = document.path()
            && self.activate_path(path, window, cx)
        {
            return;
        }
        self.add_document(document, window, cx);
    }

    /// Command-line errors go to stderr; if nothing was opened, the start screen remains.
    fn report_startup(&mut self, errors: Vec<OpenError>) {
        for error in errors {
            eprintln!("flux: {}: {}", error.path.display(), error.reason);
        }
    }

    fn report_dialog(&mut self, errors: Vec<OpenError>, cx: &mut Context<Self>) {
        if errors.is_empty() {
            return;
        }
        for error in errors {
            let notification =
                Notification::error(trf("Cannot open {0}", &[&file_name(&error.path)]))
                    .body(tr(&error.reason).to_string())
                    .group(NotificationGroup::Files);
            self.notify(notification, cx);
        }
    }

    /// A message to the user: in the active editor's status bar; under a terminal tab, in the
    /// window's status bar for a few seconds; in an empty window, under the hint.
    pub(crate) fn show_message(&mut self, message: SharedString, cx: &mut Context<Self>) {
        let editor = match self.active_item() {
            Some(TabItem::Diff(view)) => view.read(cx).working_copy().cloned(),
            _ => None,
        };
        if let Some(editor) = editor {
            return editor.update(cx, |editor, cx| editor.show_status(message, cx));
        }
        match self.active_item() {
            Some(TabItem::Editor(editor)) => {
                editor.update(cx, |editor, cx| editor.show_status(message, cx))
            }
            Some(TabItem::Terminal(_) | TabItem::Diff(_) | TabItem::Merge(_) | TabItem::Log(_)) => {
                self.status_message = Some(message);
                self.status_message_task = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(STATUS_MESSAGE_DURATION)
                        .await;
                    this.update(cx, |this, cx| {
                        this.status_message = None;
                        cx.notify();
                    })
                    .ok();
                }));
                cx.notify();
            }
            None => {
                self.notice = Some(message);
                cx.notify();
            }
        }
    }

    // --- Closing ---

    fn close_active_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        match self.active_item() {
            Some(item) => self.close_item(item, window, cx),
            // In an empty window, cmd-w closes the window itself.
            None => window.remove_window(),
        }
    }

    /// Closes a tab: a document as [`Self::close_tab`] does, a terminal tab after its terminals'
    /// running commands are confirmed.
    fn close_item(&mut self, item: TabItem, window: &mut Window, cx: &mut Context<Self>) {
        match item {
            TabItem::Editor(editor) => self.close_tab(editor, window, cx),
            TabItem::Terminal(group) => self.close_terminal_tab(group, window, cx),
            TabItem::Diff(view) => self.close_diff_tab(view, window, cx),
            TabItem::Merge(view) => self.close_merge_tab(view, window, cx),
            TabItem::Log(view) => {
                if let Some(index) = self.index_of_log(&view) {
                    self.remove_tab_at(index, window, cx);
                }
            }
        }
    }

    /// Closes a diff; the working copy it opened without a tab is asked about if it has unsaved
    /// changes.
    fn close_diff_tab(
        &mut self,
        view: Entity<DiffView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.closing {
            return;
        }
        let owned = owned_editor(&view, cx).filter(|editor| is_modified(editor, cx));
        let remove = move |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
            if let Some(index) = this
                .tabs
                .iter()
                .position(|tab| tab.item.diff() == Some(&view))
            {
                this.remove_tab_at(index, window, cx);
            }
        };
        let Some(editor) = owned else {
            return remove(self, window, cx);
        };
        let confirm = self.confirm(vec![editor], window, cx);
        cx.spawn_in(window, async move |this, cx| {
            if confirm.await {
                this.update_in(cx, |this, window, cx| remove(this, window, cx))
                    .ok();
            }
        })
        .detach();
    }

    // --- Git ---

    /// Opens the diff of a file against HEAD in a tab (or activates the one that is open). The right
    /// side is the file's editor: the one of its tab, otherwise a new one the diff holds; a deleted
    /// file has none.
    pub fn open_diff(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let target = canonical(&path);
        let open = self.tabs.iter().position(|tab| {
            tab.item.diff().is_some_and(|view| {
                let view = view.read(cx);
                // A comparison of the same file (a branch, a stash) is another tab.
                view.compares().is_none() && canonical(view.path()) == target
            })
        });
        if let Some(index) = open {
            return self.activate(index, window, cx);
        }
        let editor = self.tabs.iter().find_map(|tab| {
            let editor = tab.item.editor()?;
            let path = editor.read(cx).document.path().map(canonical)?;
            (path == target).then(|| editor.clone())
        });
        if let Some(editor) = editor {
            return self.add_diff_tab(path, Some(editor), false, window, cx);
        }
        if !path.exists() {
            return self.add_diff_tab(path, None, false, window, cx);
        }
        self.loading += 1;
        let read = cx.background_executor().spawn({
            let path = path.clone();
            async move { read_document(path) }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = read.await;
            this.update_in(cx, |this, window, cx| {
                this.loading -= 1;
                match result {
                    Ok(document) => {
                        let editor = this.new_editor(document, window, cx);
                        this.add_diff_tab(path, Some(editor), true, window, cx)
                    }
                    Err(error) => this.report_dialog(vec![error], cx),
                }
            })
            .ok();
        })
        .detach();
    }

    fn add_diff_tab(
        &mut self,
        path: PathBuf,
        working: Option<Entity<Editor>>,
        owns_working: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let git = self.git.clone();
        let view = cx.new(|cx| DiffView::new(path, git, working, owns_working, window, cx));
        let subscriptions = vec![
            cx.observe(&view, |_, _, cx| cx.notify()),
            cx.subscribe_in(&view, window, |this, _, event, window, cx| match event {
                DiffViewEvent::OpenFile(location) => {
                    this.open_location(location.clone(), true, window, cx)
                }
            }),
        ];
        self.insert_tab(TabItem::Diff(view), subscriptions, None, window, cx);
    }

    /// Opens a comparison of a file at two revisions (or a revision and the working copy) in a
    /// diff tab, or activates the one that is open.
    pub fn open_compare(
        &mut self,
        path: PathBuf,
        repo: usize,
        left: DiffSide,
        right: DiffSide,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = canonical(&path);
        let open = self.tabs.iter().position(|tab| {
            tab.item.diff().is_some_and(|view| {
                let view = view.read(cx);
                canonical(view.path()) == target && view.compares() == Some((&left, &right))
            })
        });
        if let Some(index) = open {
            return self.activate(index, window, cx);
        }
        let working = (right == DiffSide::WorkingCopy)
            .then(|| {
                self.tabs.iter().find_map(|tab| {
                    let editor = tab.item.editor()?;
                    let path = editor.read(cx).document.path().map(canonical)?;
                    (path == target).then(|| editor.clone())
                })
            })
            .flatten();
        let needs_editor = right == DiffSide::WorkingCopy && working.is_none() && path.exists();
        if !needs_editor {
            return self.add_compare_tab(path, repo, left, right, working, false, window, cx);
        }
        self.loading += 1;
        let read = cx.background_executor().spawn({
            let path = path.clone();
            async move { read_document(path) }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = read.await;
            this.update_in(cx, |this, window, cx| {
                this.loading -= 1;
                match result {
                    Ok(document) => {
                        let editor = this.new_editor(document, window, cx);
                        this.add_compare_tab(
                            path,
                            repo,
                            left,
                            right,
                            Some(editor),
                            true,
                            window,
                            cx,
                        )
                    }
                    Err(error) => this.report_dialog(vec![error], cx),
                }
            })
            .ok();
        })
        .detach();
    }

    #[allow(clippy::too_many_arguments)]
    fn add_compare_tab(
        &mut self,
        path: PathBuf,
        repo: usize,
        left: DiffSide,
        right: DiffSide,
        working: Option<Entity<Editor>>,
        owns_working: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let git = self.git.clone();
        let view = cx.new(|cx| {
            let mut view = DiffView::compare(path, repo, left, right, git, working, window, cx);
            view.set_owns_working(owns_working);
            view
        });
        let subscriptions = vec![
            cx.observe(&view, |_, _, cx| cx.notify()),
            cx.subscribe_in(&view, window, |this, _, event, window, cx| match event {
                DiffViewEvent::OpenFile(location) => {
                    this.open_location(location.clone(), true, window, cx)
                }
            }),
        ];
        self.insert_tab(TabItem::Diff(view), subscriptions, None, window, cx);
    }

    /// Opens the merge tool for a conflicted file (or activates its tab).
    pub fn open_merge(
        &mut self,
        repo: usize,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = canonical(&path);
        let open = self.tabs.iter().position(|tab| {
            tab.item
                .merge()
                .is_some_and(|view| canonical(view.read(cx).path()) == target)
        });
        if let Some(index) = open {
            return self.activate(index, window, cx);
        }
        // The user is resolving now: cards that only point at the conflicts go.
        self.dismiss_notifications(&["git::ResolveConflicts"], cx);
        let git = self.git.clone();
        let view = cx.new(|cx| MergeView::new(repo, path, git, window, cx));
        let subscriptions = vec![
            cx.observe(&view, |_, _, cx| cx.notify()),
            cx.subscribe_in(&view, window, |this, view, event, window, cx| match event {
                MergeViewEvent::Close => {
                    if let Some(index) = this
                        .tabs
                        .iter()
                        .position(|tab| tab.item.merge() == Some(view))
                    {
                        this.remove_tab_at(index, window, cx);
                    }
                }
                MergeViewEvent::OpenFile(location) => {
                    this.open_location(location.clone(), true, window, cx)
                }
            }),
        ];
        self.insert_tab(TabItem::Merge(view), subscriptions, None, window, cx);
    }

    /// Closes the merge tool; an unapplied result is asked about first.
    fn close_merge_tab(
        &mut self,
        view: Entity<MergeView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.closing {
            return;
        }
        let confirm = view.update(cx, |view, cx| view.confirm_close(window, cx));
        cx.spawn_in(window, async move |this, cx| {
            if confirm.await {
                this.update_in(cx, |this, window, cx| {
                    if let Some(index) = this
                        .tabs
                        .iter()
                        .position(|tab| tab.item.merge() == Some(&view))
                    {
                        this.remove_tab_at(index, window, cx);
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    /// The Stash tab of the commit window (Unstash Changes…).
    pub fn show_stash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_commit_window(false, window, cx);
        if let Some(handle) = &self.commit_panel {
            handle
                .panel
                .update(cx, |panel, cx| panel.show_stash(window, cx));
        }
    }

    /// ⌘K (`focus_message`) and ⌘0: shows the commit window in place of the tree.
    pub fn show_commit_window(
        &mut self,
        focus_message: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = self.commit_panel(window, cx);
        if !self.commit_open {
            self.tree_before_commit = self.tree_open;
            self.tree_open = false;
            self.commit_open = true;
        }
        panel.update(cx, |panel, cx| {
            if focus_message {
                panel.focus_message(window, cx)
            } else {
                panel.focus_changes(window, cx)
            }
        });
        cx.notify();
    }

    /// ⌘0, as the Commit tool window in JetBrains IDEs: shows the commit window and focuses it; from
    /// the commit window, hides it (the tree comes back if it was there).
    pub fn toggle_commit_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self
            .commit_panel
            .as_ref()
            .is_some_and(|handle| handle.panel.read(cx).contains_focus(window, cx));
        if self.commit_open && focused {
            return self.hide_commit_window(window, cx);
        }
        self.show_commit_window(false, window, cx)
    }

    fn hide_commit_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self
            .commit_panel
            .as_ref()
            .is_some_and(|handle| handle.panel.read(cx).contains_focus(window, cx));
        self.commit_open = false;
        self.tree_open = self.tree_before_commit;
        if focused {
            self.focus_active(window, cx);
        }
        self.revealed = None;
        self.reveal_active(cx);
        cx.notify();
    }

    /// The commit window, made on first use.
    pub(crate) fn commit_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<CommitPanel> {
        if let Some(handle) = &self.commit_panel {
            return handle.panel.clone();
        }
        let git = self.git.clone();
        let width = self.left_width.clone();
        let panel = cx.new(|cx| CommitPanel::new(git, width, window, cx));
        let subscription =
            cx.subscribe_in(&panel, window, |this, _, event, window, cx| match event {
                CommitPanelEvent::OpenDiff(path) => this.open_diff(path.clone(), window, cx),
                CommitPanelEvent::OpenFile(path) => this.open_file(path.clone(), true, window, cx),
                CommitPanelEvent::FocusEditor => this.focus_active(window, cx),
                CommitPanelEvent::Push => crate::push_dialog::open(this, window, cx),
                CommitPanelEvent::Removed(paths) => this.documents_removed(paths, window, cx),
            });
        self.commit_panel = Some(CommitPanelHandle {
            panel: panel.clone(),
            _subscription: subscription,
        });
        panel
    }

    /// Closes a terminal tab (× on the tab, ⌘W with focus outside its terminals); a running command
    /// is asked about first. Its terminals end with it.
    fn close_terminal_tab(
        &mut self,
        group: Entity<TerminalGroup>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.closing {
            return;
        }
        let confirm = group.update(cx, |group, cx| group.confirm_close(window, cx));
        cx.spawn_in(window, async move |this, cx| {
            if confirm.await {
                this.update_in(cx, |this, window, cx| {
                    if let Some(index) = this.index_of_terminal(&group) {
                        this.remove_tab_at(index, window, cx);
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    /// Closes a tab; a modified one only after the save prompt is answered.
    fn close_tab(&mut self, editor: Entity<Editor>, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        if !is_modified(&editor, cx) {
            return self.remove_tab(&editor, window, cx);
        }
        let confirm = self.confirm(vec![editor.clone()], window, cx);
        cx.spawn_in(window, async move |this, cx| {
            if confirm.await {
                this.update_in(cx, |this, window, cx| this.remove_tab(&editor, window, cx))
                    .ok();
            }
        })
        .detach();
    }

    fn close_window(&mut self, _: &CloseWindow, window: &mut Window, cx: &mut Context<Self>) {
        if self.request_close_window(window, cx) {
            window.remove_window();
        }
    }

    /// `true` means the window can be closed right away. Otherwise asks about the running commands
    /// and the unsaved documents and closes the window itself unless the user cancels.
    fn request_close_window(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.closing {
            return false;
        }
        if self.running_processes(cx).is_empty()
            && !self.tabs.iter().any(|tab| tab.item.is_modified(cx))
        {
            return true;
        }
        let confirm = self.confirm_close_all(window, cx);
        cx.spawn_in(window, async move |_, cx| {
            if confirm.await {
                cx.update(|window, _| window.remove_window()).ok();
            }
        })
        .detach();
        false
    }

    /// Before the window closes or the app quits: one question about the commands running in the
    /// window's terminals (the panel's and the tabs'), then the unsaved documents one by one, then
    /// the merge results not applied yet. `true` means everything may go.
    fn confirm_close_all(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
        let documents = self.confirm_close_documents(window, cx);
        let merges: Vec<Entity<MergeView>> = self
            .tabs
            .iter()
            .filter_map(|tab| tab.item.merge().cloned())
            .filter(|view| view.read(cx).is_modified(cx))
            .collect();
        if merges.is_empty() {
            return documents;
        }
        cx.spawn_in(window, async move |this, cx| {
            if !documents.await {
                return false;
            }
            for view in merges {
                let shown = this.update_in(cx, |this, window, cx| {
                    if let Some(index) = this
                        .tabs
                        .iter()
                        .position(|tab| tab.item.merge() == Some(&view))
                    {
                        this.activate(index, window, cx);
                    }
                    view.update(cx, |view, cx| view.confirm_close(window, cx))
                });
                let Ok(confirm) = shown else {
                    return false;
                };
                if !confirm.await {
                    return false;
                }
            }
            true
        })
    }

    /// The running commands and the unsaved documents of [`Self::confirm_close_all`].
    fn confirm_close_documents(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        if self.closing {
            return Task::ready(false);
        }
        let editors = self.editors(cx);
        let Some(detail) = running_processes_detail(&self.running_processes(cx)) else {
            return self.confirm(editors, window, cx);
        };
        self.closing = true;
        window.activate_window();
        let answer = Dialog::warning(tr("Terminate running processes?"))
            .message(detail)
            .danger(tr("Terminate"))
            .cancel(tr("Cancel"))
            .show(window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let terminate = answer.await == Some(0);
            this.update(cx, |this, _| this.closing = false).ok();
            if !terminate {
                return false;
            }
            let Ok(confirm) =
                this.update_in(cx, |this, window, cx| this.confirm(editors, window, cx))
            else {
                return false;
            };
            confirm.await
        })
    }

    /// Names of the commands running in the window's terminals: in the panel and in the tabs.
    fn running_processes(&self, cx: &App) -> Vec<String> {
        self.terminal_groups(cx)
            .iter()
            .flat_map(|group| group.read(cx).running_processes(cx))
            .collect()
    }

    /// All terminal tabs of the window: the panel's, then those in the editor area.
    fn terminal_groups(&self, cx: &App) -> Vec<Entity<TerminalGroup>> {
        let mut groups = self.terminal_panel.read(cx).groups();
        groups.extend(
            self.tabs
                .iter()
                .filter_map(|tab| tab.item.terminal().cloned()),
        );
        groups
    }

    /// Asks about each modified document in `editors` in turn, showing its tab. `true` means all
    /// are resolved (saved or discarded); `false` means canceled, a save error, or another review
    /// already in progress.
    fn confirm(
        &mut self,
        editors: Vec<Entity<Editor>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        if self.closing {
            return Task::ready(false);
        }
        let modified: Vec<_> = editors.into_iter().filter(|e| is_modified(e, cx)).collect();
        if modified.is_empty() {
            return Task::ready(true);
        }
        self.closing = true;
        window.activate_window();
        cx.spawn_in(window, async move |this, cx| {
            let confirmed = ask_each(&this, modified, cx).await;
            this.update(cx, |this, _| this.closing = false).ok();
            confirmed
        })
    }

    /// cmd-f / cmd-alt-f: the find bar for the active tab. A terminal tab searches its output
    /// itself (the terminal handles cmd-f when it has focus): focus goes there with the request —
    /// unless it came from there, unhandled.
    fn deploy_find(&mut self, replace: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(group) = self.active_terminal() {
            if !contains_focus(&group, window, cx) {
                window.focus(&group.focus_handle(cx));
                window.dispatch_action(find_bar::Deploy.boxed_clone(), cx);
            }
            return;
        }
        let editor = self.active_editor();
        self.find_bar
            .update(cx, |bar, cx| bar.deploy(replace, editor, window, cx));
    }

    // --- File tree ---

    fn build_tree(
        root: PathBuf,
        width: ui::LeftIslandWidth,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> TreePanel {
        let panel = cx.new(|cx| FileTreePanel::new(root, width, window, cx));
        let subscription = cx.subscribe_in(&panel, window, Self::on_tree_event);
        TreePanel {
            panel,
            _subscription: subscription,
        }
    }

    fn on_tree_event(
        &mut self,
        _: &Entity<FileTreePanel>,
        event: &FileTreeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            FileTreeEvent::Open { path, focus } => self.open_file(path.clone(), *focus, window, cx),
            FileTreeEvent::FocusEditor => self.focus_active(window, cx),
            FileTreeEvent::Moved { from, to } => self.documents_moved(from, to, cx),
            FileTreeEvent::Removed { paths } => self.documents_removed(paths, window, cx),
            FileTreeEvent::Error { title, body } => {
                let notification = Notification::error(title.clone())
                    .body(body.clone())
                    .group(NotificationGroup::Files);
                self.notify(notification, cx)
            }
            FileTreeEvent::DiskChanged(paths) => self.sync_documents(paths.clone(), cx),
        }
    }

    /// Whether a launchpad tool window is open: its button is highlighted.
    pub(crate) fn tool_open(&self, tool: Tool, cx: &App) -> bool {
        match tool {
            Tool::Project => self.tree_open && self.file_tree.is_some(),
            Tool::Commit => self.commit_open,
            Tool::FindInFiles => self.project_search.read(cx).is_open(),
            Tool::Terminal => self.terminal_open && !self.git_open,
            Tool::Git => self.git_open,
            Tool::Notifications => self.right_tool == Some(RightTool::Notifications),
        }
    }

    /// A count on a launchpad tool: the number of changes on Commit.
    pub(crate) fn tool_badge(&self, tool: Tool, cx: &App) -> Option<usize> {
        match tool {
            Tool::Commit => Some(self.git.read(cx).change_count()).filter(|count| *count > 0),
            Tool::Notifications => {
                Some(self.notification_center.read(cx).unread_count()).filter(|count| *count > 0)
            }
            _ => None,
        }
    }

    /// The badge of a tool is an alarm (the error color): unread error notifications.
    pub(crate) fn tool_badge_alarm(&self, tool: Tool, cx: &App) -> bool {
        tool == Tool::Notifications
            && self.notification_center.read(cx).unread_kind()
                == Some(crate::notifications::NotificationKind::Error)
    }

    // --- The island on the right ---

    /// The launchpad, the bell, the palette: shows the window focused, or hides it (with no shortcut,
    /// it is reached by the mouse — a second click hides, whatever has the focus).
    fn toggle_notifications(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.right_tool == Some(RightTool::Notifications) {
            return self.hide_right(window, cx);
        }
        self.show_right(RightTool::Notifications, window, cx);
    }

    /// A plugin's tool window: its launchpad icon, its keys, the palette — shows it focused, or
    /// hides it.
    pub(crate) fn toggle_plugin_tool(
        &mut self,
        key: ToolKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.right_tool == Some(RightTool::Plugin(key)) {
            return self.hide_right(window, cx);
        }
        self.show_right(RightTool::Plugin(key), window, cx);
    }

    /// Whether a plugin's tool window is shown: its launchpad icon is highlighted.
    pub(crate) fn plugin_tool_open(&self, key: ToolKey) -> bool {
        self.right_tool == Some(RightTool::Plugin(key))
    }

    fn plugin_tool_view(
        &self,
        key: ToolKey,
        cx: &App,
    ) -> Option<Entity<crate::plugin_view::PluginView>> {
        self.plugins
            .read(cx)
            .tool_window(key)
            .map(|tool| tool.view.clone())
    }

    /// What the plugins hub tells the window.
    fn on_plugin_store_event(
        &mut self,
        _: &Entity<PluginStore>,
        event: &PluginStoreEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            PluginStoreEvent::Changed => {
                // A plugin that went away takes its tool window with it.
                if let Some(RightTool::Plugin(key)) = self.right_tool
                    && self.plugin_tool_view(key, cx).is_none()
                {
                    self.right_tool = None;
                    self.focus_active(window, cx);
                }
                cx.notify();
            }
            PluginStoreEvent::ShowToolWindow(key) => {
                if self.right_tool != Some(RightTool::Plugin(*key)) {
                    self.show_right(RightTool::Plugin(*key), window, cx);
                }
            }
            PluginStoreEvent::HideToolWindow(key) => {
                if self.right_tool == Some(RightTool::Plugin(*key)) {
                    self.hide_right(window, cx);
                }
            }
            PluginStoreEvent::Call { plugin, call } => {
                plugins::handle_call(self, plugin, call, window, cx)
            }
        }
    }

    fn show_right(&mut self, tool: RightTool, window: &mut Window, cx: &mut Context<Self>) {
        // One window at a time in the island: the one shown before goes.
        if self.right_tool.is_some_and(|shown| shown != tool) {
            self.hide_right(window, cx);
        }
        self.right_tool = Some(tool);
        match tool {
            RightTool::Notifications => {
                self.notification_center
                    .update(cx, |center, cx| center.set_journal_in_sight(true, cx));
                self.notifications_panel
                    .update(cx, |panel, cx| panel.set_visible(true, cx));
                window.focus(&self.notifications_panel.focus_handle(cx));
            }
            RightTool::Plugin(key) => {
                let Some(view) = self.plugin_tool_view(key, cx) else {
                    self.right_tool = None;
                    return;
                };
                view.update(cx, |view, cx| view.set_visible(true, cx));
                window.focus(&view.focus_handle(cx));
                self.plugins
                    .update(cx, |store, _| store.tool_window_visibility(key, true));
            }
        }
        cx.notify();
    }

    /// ⇧Esc in the window, its hide button, a second click: the island goes, and the focus, if it
    /// was there, goes to the editor.
    fn hide_right(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tool) = self.right_tool.take() else {
            return;
        };
        let focused = match tool {
            RightTool::Notifications => {
                self.notification_center
                    .update(cx, |center, cx| center.set_journal_in_sight(false, cx));
                let focused = self
                    .notifications_panel
                    .read(cx)
                    .contains_focus(window, cx);
                self.notifications_panel
                    .update(cx, |panel, cx| panel.set_visible(false, cx));
                focused
            }
            RightTool::Plugin(key) => {
                self.plugins
                    .update(cx, |store, _| store.tool_window_visibility(key, false));
                match self.plugin_tool_view(key, cx) {
                    Some(view) => {
                        let focused = view.read(cx).contains_focus(window, cx);
                        view.update(cx, |view, cx| view.set_visible(false, cx));
                        focused
                    }
                    None => false,
                }
            }
        };
        if focused || window.focused(cx).is_none() {
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    fn tree_panel(&self) -> Option<Entity<FileTreePanel>> {
        self.file_tree.as_ref().map(|tree| tree.panel.clone())
    }

    /// The tree follows the active tab: its file is revealed and selected. Called when the tab
    /// changes and on any editor change; the tree is touched only when the file has changed.
    fn reveal_active(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = self.tree_panel().filter(|_| self.tree_open) else {
            return;
        };
        let path = self
            .active_editor()
            .and_then(|editor| editor.read(cx).document.path().map(Path::to_path_buf));
        if path == self.revealed {
            return;
        }
        if let Some(path) = &path {
            panel.update(cx, |panel, cx| panel.reveal(path, cx));
        }
        self.revealed = path;
    }

    /// cmd-1: show or hide the file tree.
    fn toggle_tree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.tree_panel() else {
            return self.show_message(tr("No project folder — open one with ⌘O").into(), cx);
        };
        let focused = panel.focus_handle(cx).contains_focused(window, cx);
        // The commit window gives its place back to the tree, and its focus with it: a hidden panel
        // that keeps focus would take the keys away — gpui finds no path from it to the window, and
        // ⌘0, ⌘K and the rest go nowhere.
        if self.commit_open {
            let commit_focused = self
                .commit_panel
                .as_ref()
                .is_some_and(|handle| handle.panel.read(cx).contains_focus(window, cx));
            self.commit_open = false;
            self.tree_open = true;
            if commit_focused {
                window.focus(&panel.focus_handle(cx));
            }
            self.revealed = None;
            self.reveal_active(cx);
            return cx.notify();
        }
        self.tree_open = !self.tree_open;
        if !self.tree_open && focused {
            self.focus_active(window, cx);
        }
        self.revealed = None;
        self.reveal_active(cx);
        cx.notify();
    }

    /// cmd-shift-e: move focus to the file tree (showing it first); from the tree, back to the
    /// editor.
    fn toggle_tree_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.tree_panel() else {
            return self.show_message(tr("No project folder — open one with ⌘O").into(), cx);
        };
        let handle = panel.focus_handle(cx);
        if self.tree_open && handle.contains_focused(window, cx) {
            return self.focus_active(window, cx);
        }
        self.commit_open = false;
        if !self.tree_open {
            self.tree_open = true;
            self.revealed = None;
            self.reveal_active(cx);
        }
        window.focus(&handle);
        cx.notify();
    }

    /// Files changed on disk (`None` — any may have: the window was activated, events were lost):
    /// an open document without unsaved changes takes the new content, as one edit that ⌘Z undoes;
    /// one with unsaved changes keeps them, and the status bar says the file changed.
    fn sync_documents(&mut self, paths: Option<Vec<PathBuf>>, cx: &mut Context<Self>) {
        let editors: Vec<(Entity<Editor>, PathBuf)> = self
            .editors(cx)
            .into_iter()
            .filter_map(|editor| {
                let path = editor.read(cx).document.path()?.to_path_buf();
                let affected = paths.as_ref().is_none_or(|paths| paths.contains(&path));
                affected.then_some((editor, path))
            })
            .collect();
        if editors.is_empty() {
            return;
        }
        let known: Vec<Option<std::time::SystemTime>> = editors
            .iter()
            .map(|(editor, _)| editor.read(cx).disk_mtime)
            .collect();
        let paths: Vec<PathBuf> = editors.iter().map(|(_, path)| path.clone()).collect();
        // Only files written since the editor last saw them are read; unreadable or missing ones are
        // left alone: deletion has its own path (the tree, git).
        let read = cx.background_spawn(async move {
            paths
                .into_iter()
                .zip(known)
                .map(|(path, known)| {
                    let mtime = editor::file_mtime(&path)?;
                    if Some(mtime) == known {
                        return None;
                    }
                    Some((mtime, fs::read_to_string(&path).ok()?))
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let contents = read.await;
            this.update(cx, |this, cx| {
                for ((editor, _), read) in editors.into_iter().zip(contents) {
                    let Some((mtime, content)) = read else {
                        continue;
                    };
                    let (same, modified) = {
                        let document = &editor.read(cx).document;
                        (document.text() == content.as_str(), document.is_modified())
                    };
                    editor.update(cx, |editor, _| editor.disk_mtime = Some(mtime));
                    if same {
                        continue;
                    }
                    if modified {
                        let name = editor.read(cx).document.display_name();
                        let notification =
                            Notification::warning(trf("“{0}” changed on disk", &[&name]))
                                .body(tr("Your unsaved changes are kept"))
                                .group(NotificationGroup::Files);
                        this.notify(notification, cx);
                        continue;
                    }
                    editor.update(cx, |editor, cx| editor.reload(&content, cx));
                }
            })
            .ok();
        })
        .detach();
    }

    /// A file or directory was moved (renamed, or moved within the tree): the open documents inside
    /// it are switched to the new paths.
    fn documents_moved(&mut self, from: &Path, to: &Path, cx: &mut Context<Self>) {
        for editor in self.editors(cx) {
            let moved = editor
                .read(cx)
                .document
                .path()
                .and_then(|path| remap(path, from, to));
            if let Some(path) = moved {
                editor.update(cx, |editor, cx| editor.set_path(path, cx));
            }
        }
    }

    /// Files were moved to the Trash: their tabs are closed. Modified ones stay open, so edits are
    /// not lost; saving will recreate the file.
    fn documents_removed(
        &mut self,
        paths: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Closing a tab activates the adjacent one and moves focus into it, but here the deletion
        // came from the tree, so focus must stay there (unless it was in the closed tab).
        let mut keep_focus = window.focused(cx);
        let mut kept = false;
        for editor in self.editors(cx) {
            let (removed, modified) = {
                let document = &editor.read(cx).document;
                let removed = document
                    .path()
                    .is_some_and(|path| paths.iter().any(|removed| path.starts_with(removed)));
                (removed, document.is_modified())
            };
            match (removed, modified) {
                (true, true) => kept = true,
                (true, false) => {
                    if keep_focus.as_ref() == Some(&editor.focus_handle(cx)) {
                        keep_focus = None;
                    }
                    self.remove_tab(&editor, window, cx)
                }
                _ => {}
            }
        }
        restore_focus(keep_focus, window);
        if kept {
            let notification = Notification::info(tr(
                "Deleted files with unsaved changes stay open — save to restore them",
            ))
            .group(NotificationGroup::Files);
            self.notify(notification, cx);
        }
    }

    // --- Git window ---

    /// The Git window, made on first use.
    fn git_window(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<GitWindow> {
        if let Some(handle) = &self.git_window {
            return handle.window.clone();
        }
        let git = self.git.clone();
        let height = self.bottom_height.clone();
        let git_window = cx.new(|cx| GitWindow::new(git, height, window, cx));
        let subscriptions = [
            cx.observe(&git_window, |_, _, cx| cx.notify()),
            cx.subscribe_in(&git_window, window, Self::on_git_window_event),
        ];
        self.git_window = Some(GitWindowHandle {
            window: git_window.clone(),
            _subscriptions: subscriptions,
        });
        git_window
    }

    fn on_git_window_event(
        &mut self,
        _: &Entity<GitWindow>,
        event: &GitWindowEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            GitWindowEvent::Empty => self.hide_git_window(window, cx),
            GitWindowEvent::MoveToPanel { view, index } => {
                self.move_log_to_panel(view.clone(), Some(*index), window, cx)
            }
        }
    }

    /// The repository the log starts with: the active file's, or the first one.
    fn log_repo(&self, cx: &App) -> Option<usize> {
        let git = self.git.read(cx);
        if git.repos().is_empty() {
            return None;
        }
        Some(
            self.active_path(cx)
                .and_then(|path| git.repo_index(&path))
                .unwrap_or(0),
        )
    }

    /// The repository log's view: in the Git window, or a tab of the editor area; made (in the
    /// window) if there is none.
    fn log_view(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<GitLogView>> {
        if let Some(view) = self
            .tabs
            .iter()
            .filter_map(|tab| tab.item.log())
            .find(|view| *view.read(cx).scope() == crate::git_log::LogScope::All)
        {
            return Some(view.clone());
        }
        let git_window = self.git_window(window, cx);
        if let Some(view) = git_window.read(cx).log_view(cx) {
            return Some(view);
        }
        let repo = self.log_repo(cx)?;
        Some(git_window.update(cx, |git_window, cx| git_window.new_log(repo, window, cx)))
    }

    /// Shows a view of the Git window: its tab in the editor area, or the window with it.
    fn reveal_log(
        &mut self,
        view: &Entity<GitLogView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self.index_of_log(view) {
            return self.activate(index, window, cx);
        }
        let git_window = self.git_window(window, cx);
        self.git_open = true;
        git_window.update(cx, |git_window, cx| {
            git_window.activate_view(view, window, cx)
        });
        cx.notify();
    }

    /// ⌘9, as the Git tool window of JetBrains IDEs: shows the window with the log focused; from
    /// the window, hides it.
    fn toggle_git_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self
            .git_window
            .as_ref()
            .is_some_and(|handle| handle.window.read(cx).contains_focus(window, cx));
        if self.git_open && focused {
            return self.hide_git_window(window, cx);
        }
        if self.git.read(cx).repos().is_empty() && self.git_window.is_none() {
            return self.show_message(tr("No Git repository in the project").into(), cx);
        }
        let git_window = self.git_window(window, cx);
        if git_window.read(cx).is_empty() {
            match self.log_view(window, cx) {
                // The log lives in the editor area: the window stays as it is, the tab comes forward.
                Some(view) if self.index_of_log(&view).is_some() => {
                    return self.reveal_log(&view, window, cx);
                }
                _ => {}
            }
        }
        self.git_open = true;
        git_window.update(cx, |git_window, cx| git_window.focus(window, cx));
        cx.notify();
    }

    /// ⇧Esc in the Git window, or its last tab gone: the window hides, focus goes to the editor.
    fn hide_git_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self
            .git_window
            .as_ref()
            .is_some_and(|handle| handle.window.read(cx).contains_focus(window, cx));
        self.git_open = false;
        if focused || window.focused(cx).is_none() {
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    /// Show Commit in Log: the log of the repository with the commit selected.
    fn show_commit_in_log(
        &mut self,
        repo: usize,
        oid: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.log_view(window, cx) else {
            return;
        };
        self.reveal_log(&view, window, cx);
        view.update(cx, |view, cx| {
            if view.repo() != repo {
                window.dispatch_action(Box::new(crate::git_log::SetRepo { repo }), cx);
            }
            view.show_commit(oid, window, cx)
        });
    }

    /// The history of a file or of its lines: a tab of the Git window.
    fn show_history(
        &mut self,
        path: &Path,
        lines: Option<(u32, u32)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A history tab already in the editor area comes forward.
        let git_window = self.git_window(window, cx);
        let view = git_window.update(cx, |git_window, cx| {
            git_window.show_history(path, lines, window, cx)
        });
        match view {
            Some(_) => {
                self.git_open = true;
                cx.notify();
            }
            None => self.show_message(tr("The file isn’t in a Git repository").into(), cx),
        }
    }

    fn index_of_log(&self, view: &Entity<GitLogView>) -> Option<usize> {
        self.tabs
            .iter()
            .position(|tab| tab.item.log() == Some(view))
    }

    /// A tab of the Git window moves to the editor area at `index` (by default, next to the active
    /// tab) and gets focus. A window left empty hides.
    fn move_log_to_editor(
        &mut self,
        view: Entity<GitLogView>,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(handle) = &self.git_window else {
            return;
        };
        let git_window = handle.window.clone();
        if !git_window.update(cx, |git_window, cx| git_window.remove_view(&view, cx)) {
            return;
        }
        if git_window.read(cx).is_empty() {
            self.git_open = false;
        }
        let subscriptions = vec![cx.observe(&view, |_, _, cx| cx.notify())];
        self.insert_tab(TabItem::Log(view), subscriptions, index, window, cx);
    }

    /// "Move to Editor" in the Git window: its active tab.
    fn move_log_tab_to_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = self
            .git_window
            .as_ref()
            .and_then(|handle| handle.window.read(cx).active_view());
        if let Some(view) = view {
            self.move_log_to_editor(view, None, window, cx);
        }
    }

    /// A tab of the editor area moves back to the Git window at `index` (by default, at the end).
    fn move_log_to_panel(
        &mut self,
        view: Entity<GitLogView>,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.index_of_log(&view) else {
            return;
        };
        self.remove_tab_at(tab, window, cx);
        let git_window = self.git_window(window, cx);
        self.git_open = true;
        git_window.update(cx, |git_window, cx| {
            git_window.add_view(view, index, window, cx)
        });
        cx.notify();
    }

    /// A Git window tab dropped on the tab strip: one already here moves along it.
    fn drop_log(&mut self, view: Entity<GitLogView>, window: &mut Window, cx: &mut Context<Self>) {
        let index = self.tab_drop.take().unwrap_or(self.tabs.len());
        match self.index_of_log(&view) {
            Some(from) => self.move_tab(from, index, window, cx),
            None => self.move_log_to_editor(view, Some(index), window, cx),
        }
    }

    // --- Terminal ---

    /// ⌥F12, as the Terminal tool window in JetBrains IDEs: shows the panel and focuses the
    /// terminal (starting one in an empty panel); from the terminal, hides the panel.
    fn toggle_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.terminal_open && self.terminal_panel.read(cx).contains_focus(window, cx) {
            return self.hide_terminal(window, cx);
        }
        self.terminal_open = true;
        self.git_open = false;
        self.terminal_panel
            .update(cx, |panel, cx| panel.focus(window, cx));
        cx.notify();
    }

    /// ⇧Esc in the terminal, or the last terminal closed: the panel hides, focus goes to the editor.
    fn hide_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.terminal_panel.read(cx).contains_focus(window, cx);
        self.terminal_open = false;
        if focused || window.focused(cx).is_none() {
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    /// ⌘T: a new terminal tab in the panel.
    fn new_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.terminal_open = true;
        self.git_open = false;
        self.terminal_panel
            .update(cx, |panel, cx| panel.new_terminal(window, cx));
        cx.notify();
    }

    fn on_terminal_panel_event(
        &mut self,
        _: &Entity<TerminalPanel>,
        event: &TerminalPanelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TerminalPanelEvent::Empty => self.hide_terminal(window, cx),
            TerminalPanelEvent::OpenLink(link) => self.open_terminal_link(link, window, cx),
            TerminalPanelEvent::MoveToPanel { group, index } => {
                self.move_terminal_to_panel(group.clone(), Some(*index), window, cx)
            }
            TerminalPanelEvent::ShellFailed(reason) => self.shell_failed(reason, cx),
        }
    }

    /// A terminal's shell didn't start (a broken `$SHELL`): an error notification.
    fn shell_failed(&mut self, reason: &SharedString, cx: &mut Context<Self>) {
        let notification = Notification::error(tr("Couldn't start the shell"))
            .body(reason.clone())
            .group(NotificationGroup::Terminal);
        self.notify(notification, cx)
    }

    /// Events of a terminal tab in the editor area.
    fn on_terminal_tab_event(
        &mut self,
        group: &Entity<TerminalGroup>,
        event: &TerminalGroupEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            // Its last terminal ended: the tab goes away.
            TerminalGroupEvent::Empty => {
                if let Some(index) = self.index_of_terminal(group) {
                    self.remove_tab_at(index, window, cx);
                }
            }
            TerminalGroupEvent::TitleChanged => cx.notify(),
            // The tab in front is being looked at: no mark for it.
            TerminalGroupEvent::Bell => {
                if self.active_terminal().as_ref() == Some(group) {
                    group.update(cx, |group, cx| group.clear_bell(cx));
                }
                cx.notify();
            }
            TerminalGroupEvent::OpenLink(link) => self.open_terminal_link(link, window, cx),
            TerminalGroupEvent::ShellFailed(reason) => self.shell_failed(reason, cx),
        }
    }

    /// A terminal tab moves from the panel to the editor area at `index` (by default, next to the
    /// active tab) and gets focus. A panel left empty hides.
    fn move_terminal_to_editor(
        &mut self,
        group: Entity<TerminalGroup>,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let removed = self
            .terminal_panel
            .update(cx, |panel, cx| panel.remove_group(&group, cx));
        if !removed {
            return;
        }
        if self.terminal_panel.read(cx).is_empty() {
            self.terminal_open = false;
        }
        self.add_terminal_tab(group, index, window, cx);
    }

    /// A terminal tab moves from the editor area to the panel at `index` (by default, after the
    /// panel's active tab); the panel shows and the tab gets focus.
    fn move_terminal_to_panel(
        &mut self,
        group: Entity<TerminalGroup>,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.index_of_terminal(&group) else {
            return;
        };
        self.remove_tab_at(tab, window, cx);
        self.terminal_open = true;
        self.terminal_panel
            .update(cx, |panel, cx| panel.add_group(group, index, window, cx));
        cx.notify();
    }

    /// The terminal tab that has focus, in the panel or in the editor area.
    fn focused_terminal(&self, window: &Window, cx: &App) -> Option<Entity<TerminalGroup>> {
        self.terminal_groups(cx)
            .into_iter()
            .find(|group| contains_focus(group, window, cx))
    }

    /// "Move to Editor" in the panel: the focused terminal tab, or the panel's active one.
    fn move_to_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let panel = self.terminal_panel.read(cx);
        let group = self
            .focused_terminal(window, cx)
            .filter(|group| panel.groups().contains(group))
            .or_else(|| panel.active_group());
        if let Some(group) = group {
            self.move_terminal_to_editor(group, None, window, cx);
        }
    }

    /// "Move to Panel" in a terminal of the editor area: its tab, or the active tab if it is a
    /// terminal.
    fn move_to_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let group = self
            .focused_terminal(window, cx)
            .filter(|group| self.index_of_terminal(group).is_some())
            .or_else(|| self.active_terminal());
        if let Some(group) = group {
            self.move_terminal_to_panel(group, None, window, cx);
        }
    }

    /// ⌘-click on a link in a terminal: a file opens at its line and column, a directory is shown
    /// in the tree, a URL opens in the browser.
    fn open_terminal_link(
        &mut self,
        link: &TerminalLink,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match link {
            TerminalLink::File { path, line, column } => match line {
                Some(line) => {
                    let column = column.unwrap_or(1).saturating_sub(1) as usize;
                    let location = Location {
                        path: path.clone(),
                        line: (*line as usize).saturating_sub(1),
                        start: column,
                        end: column,
                    };
                    self.open_location(location, true, window, cx)
                }
                None => self.open_file(path.clone(), true, window, cx),
            },
            TerminalLink::Directory(path) => {
                if let Some(panel) = self.tree_panel() {
                    // The commit window gives its place to the tree, which shows the directory.
                    self.commit_open = false;
                    self.tree_open = true;
                    self.revealed = None;
                    panel.update(cx, |panel, cx| panel.reveal(path, cx));
                    cx.notify();
                }
            }
            TerminalLink::Url(url) => cx.open_url(url),
        }
    }

    // --- Overlay windows ---

    /// Opens the overlay window `V`; if it is already open, closes it. Any other open window is
    /// replaced. `build` is called before focus is moved: inside it, `window.focused` is whatever
    /// had focus before (the command palette needs this to collect the actions available to it).
    pub fn toggle_modal<V: ManagedView>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        build: impl FnOnce(&mut Window, &mut Context<V>) -> V,
    ) {
        self.open_modal(ModalKind::Popup, window, cx, build)
    }

    /// [`Self::toggle_modal`] for a dialog ([`ModalKind::Dialog`]): modal to the window, centered
    /// under the title bar.
    pub fn toggle_dialog<V: ManagedView>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        build: impl FnOnce(&mut Window, &mut Context<V>) -> V,
    ) {
        self.open_modal(ModalKind::Dialog, window, cx, build)
    }

    fn open_modal<V: ManagedView>(
        &mut self,
        kind: ModalKind,
        window: &mut Window,
        cx: &mut Context<Self>,
        build: impl FnOnce(&mut Window, &mut Context<V>) -> V,
    ) {
        if let Some(modal) = &self.modal
            && modal.view.clone().downcast::<V>().is_ok()
        {
            return self.dismiss_modal(window, cx);
        }
        // Behind a dialog, the window's shortcuts don't open popups (as behind a modal dialog of
        // JetBrains IDEs); a dialog may give way to the next dialog of a flow.
        if kind == ModalKind::Popup && self.dialog_open() {
            return;
        }
        // Project search is also an overlay window: it gives way without restoring focus.
        if self.project_search.read(cx).is_open() {
            self.project_search.update(cx, |search, cx| search.hide(cx));
            if self
                .project_search
                .focus_handle(cx)
                .contains_focused(window, cx)
            {
                self.focus_active(window, cx);
            }
        }
        let previous_focus = match self.modal.take() {
            Some(modal) => modal.previous_focus,
            None => window.focused(cx),
        };
        let view = cx.new(|cx| build(window, cx));
        let focus_handle = view.focus_handle(cx);
        let mut subscriptions = vec![cx.subscribe_in(
            &view,
            window,
            |this, _, _: &DismissEvent, window, cx| this.dismiss_modal(window, cx),
        )];
        if kind == ModalKind::Popup {
            subscriptions.push(cx.on_focus_out(&focus_handle, window, |this, _, window, cx| {
                this.dismiss_modal(window, cx)
            }));
        }
        window.focus(&focus_handle);
        self.modal = Some(Modal {
            view: view.into(),
            kind,
            focus_handle,
            previous_focus,
            anchor: None,
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    /// A dialog ([`ModalKind::Dialog`]) is open: the window behind it waits.
    pub(crate) fn dialog_open(&self) -> bool {
        self.modal
            .as_ref()
            .is_some_and(|modal| modal.kind == ModalKind::Dialog)
    }

    /// The open overlay window is a `V` (the launchpad highlights the gear for Settings).
    pub(crate) fn modal_is<V: 'static>(&self) -> bool {
        self.modal
            .as_ref()
            .is_some_and(|modal| modal.view.clone().downcast::<V>().is_ok())
    }

    /// Shows the open overlay window at a window point instead of the top center: a popover at the
    /// text it is about (rename). It stays within the window.
    pub fn anchor_modal(&mut self, anchor: Point<Pixels>) {
        if let Some(modal) = &mut self.modal {
            modal.anchor = Some(anchor);
        }
    }

    /// Closes the overlay window. Focus returns to its previous place only if it was inside the
    /// window: on a click outside, focus has already moved to wherever was clicked.
    pub fn dismiss_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(modal) = self.modal.take() else {
            return;
        };
        if modal.focus_handle.contains_focused(window, cx) || window.focused(cx).is_none() {
            match modal.previous_focus {
                Some(previous) => window.focus(&previous),
                None => self.focus_active(window, cx),
            }
        }
        // A plugin's question that waited for the overlay window comes now.
        cx.defer_in(window, |this, window, cx| {
            plugins::ask_pending(this, window, cx)
        });
        cx.notify();
    }

    /// An overlay window (a popup or a dialog) is open.
    pub(crate) fn modal_open(&self) -> bool {
        self.modal.is_some()
    }

    fn render_modal(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let modal = self.modal.as_ref()?;
        let view = div()
            .when(modal.kind == ModalKind::Popup, |view| {
                view.on_mouse_down_out(
                    cx.listener(|this, _, window, cx| this.dismiss_modal(window, cx)),
                )
            })
            .child(modal.view.clone());
        if modal.kind == ModalKind::Dialog {
            // Over a backdrop that takes the clicks meant for the window behind; the title bar
            // stays free, so the window can still be moved.
            return Some(
                ui::modal_backdrop(Theme::ui(cx))
                    .id("dialog-backdrop")
                    .absolute()
                    .top(px(TITLE_BAR_HEIGHT))
                    .bottom_0()
                    .left_0()
                    .right_0()
                    .flex()
                    .justify_center()
                    .pt(px(MODAL_TOP - TITLE_BAR_HEIGHT))
                    .child(view)
                    .into_any_element(),
            );
        }
        Some(match modal.anchor {
            Some(anchor) => deferred(
                anchored()
                    .position(anchor)
                    .snap_to_window_with_margin(px(ui::GAP))
                    .child(view),
            )
            .with_priority(1)
            .into_any_element(),
            None => div()
                .absolute()
                .top(px(MODAL_TOP))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(view)
                .into_any_element(),
        })
    }

    /// Project search is a window over the islands; a click outside closes it.
    fn render_project_search(&self, cx: &Context<Self>) -> Option<impl IntoElement + use<>> {
        if !self.project_search.read(cx).is_open() {
            return None;
        }
        Some(
            div()
                .absolute()
                .top(px(TITLE_BAR_HEIGHT + GAP))
                .bottom(px(STATUS_BAR_HEIGHT + GAP))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .w(relative(SEARCH_WIDTH))
                        .max_w(px(SEARCH_MAX_WIDTH))
                        .h_full()
                        .on_mouse_down_out(
                            cx.listener(|this, _, window, cx| this.hide_project_search(window, cx)),
                        )
                        .child(self.project_search.clone()),
                ),
        )
    }

    /// Hides project search (on a click outside): focus returns to the editor only if it was still
    /// in the search; otherwise it is already wherever was clicked.
    fn hide_project_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let inside = self
            .project_search
            .focus_handle(cx)
            .contains_focused(window, cx);
        self.project_search.update(cx, |search, cx| search.hide(cx));
        if inside || window.focused(cx).is_none() {
            self.focus_active(window, cx);
        }
    }

    // --- Display ---

    /// The window title "● name — project", derived from the active tab (a terminal tab: "zsh —
    /// project"; without a project: "— Flux"), and a dot on the red button if there are unsaved
    /// changes. We call into the platform only when something has changed.
    fn update_title(&mut self, window: &mut Window, cx: &App) {
        let project = self.root.as_deref().and_then(Path::file_name).map_or_else(
            || "Flux".to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let title = match self.active_item() {
            Some(TabItem::Editor(editor)) => {
                let document = &editor.read(cx).document;
                let modified = if document.is_modified() { "● " } else { "" };
                format!("{modified}{} — {project}", document_name(document))
            }
            Some(TabItem::Terminal(group)) => format!("{} — {project}", group.read(cx).title(cx)),
            Some(TabItem::Diff(view)) => {
                trf("{0} (diff) — {1}", &[&view.read(cx).title(), &project])
            }
            Some(TabItem::Merge(view)) => {
                trf("{0} (merge) — {1}", &[&view.read(cx).title(), &project])
            }
            Some(TabItem::Log(view)) => format!("{} — {project}", view.read(cx).title()),
            None => project.clone(),
        };
        if title != self.title {
            window.set_window_title(&title);
            self.title = title;
        }
        let edited = self.tabs.iter().any(|tab| tab.item.is_modified(cx));
        if edited != self.edited {
            window.set_window_edited(edited);
            self.edited = edited;
        }
    }

    /// The title bar on the window frame: space for the traffic lights, the project and branch, the
    /// file search bar in the middle, buttons on the right. A double-click acts like one on a macOS
    /// window title bar.
    fn render_title_bar(&self, window: &Window, cx: &Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let leading = if window.is_fullscreen() {
            GAP + 4.
        } else {
            TRAFFIC_LIGHTS_WIDTH
        };
        let project = self.root.as_deref().map(|root| {
            let name = root
                .file_name()
                .map_or_else(|| tilde(root), |name| name.to_string_lossy().into_owned());
            div()
                .id("title-project")
                .flex()
                .items_center()
                .gap_1p5()
                .h(px(26.))
                .px_2()
                .rounded(px(RADIUS_SM))
                .cursor_pointer()
                .hover(move |style| style.bg(ui.hover))
                .tooltip(ui::tooltip(tilde(root), ui::shortcut_for(&Open, window)))
                .on_click(|_, window, cx| window.dispatch_action(Open.boxed_clone(), cx))
                .child(icon(IconName::Folder, ui.folder).size(px(14.)))
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(ui.foreground)
                        .child(name),
                )
        });
        let open_folder = self.root.is_none().then(|| {
            div()
                .id("title-open-folder")
                .flex()
                .items_center()
                .gap_1p5()
                .h(px(26.))
                .px_2()
                .rounded(px(RADIUS_SM))
                .cursor_pointer()
                .text_color(ui.text_muted)
                .hover(move |style| style.bg(ui.hover).text_color(ui.foreground))
                .tooltip(ui::tooltip(
                    tr("Open Folder…"),
                    ui::shortcut_for(&Open, window),
                ))
                .on_click(|_, window, cx| window.dispatch_action(Open.boxed_clone(), cx))
                .child(icon(IconName::FolderPlus, ui.folder).size(px(14.)))
                .child(tr("Open Folder…"))
        });
        let branch = crate::branches_popup::branch_chip(self, window, cx);
        let search =
            (f32::from(window.viewport_size().width) >= TITLE_SEARCH_MIN_WINDOW).then(|| {
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .id("title-search")
                            .w(px(340.))
                            .h(px(26.))
                            .px_2()
                            .flex()
                            .items_center()
                            .gap_2()
                            .rounded(px(RADIUS_MD))
                            .bg(ui.input_background)
                            .border_1()
                            .border_color(ui.input_border)
                            .cursor_pointer()
                            .text_color(ui.dim)
                            .hover(move |style| {
                                style
                                    .border_color(ui.elevated_border)
                                    .text_color(ui.text_muted)
                            })
                            .on_click(|_, window, cx| {
                                window.dispatch_action(file_finder::Toggle.boxed_clone(), cx)
                            })
                            .child(icon(IconName::Search, ui.dim).size(px(13.)))
                            .child(div().flex_1().child(tr("Search files")))
                            .children(
                                ui::shortcut_for(&file_finder::Toggle, window)
                                    .map(|keys| ui::keys(&keys, ui)),
                            ),
                    )
            });
        let button =
            |id: &'static str, name: IconName, label: &'static str, action: Box<dyn Action>| {
                let keys = ui::shortcut_for(action.as_ref(), window);
                ui::icon_button(id, name, ui)
                    .tooltip(ui::tooltip(label, keys))
                    .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
            };
        div()
            .id("title-bar")
            .relative()
            .flex_none()
            .h(px(TITLE_BAR_HEIGHT))
            .pl(px(leading))
            .pr(px(GAP + 2.))
            .flex()
            .items_center()
            .gap_2()
            .on_click(|event, window, _| {
                if event.click_count() == 2 {
                    window.titlebar_double_click();
                }
            })
            .children(search)
            .children(project)
            .children(open_folder)
            .children(branch)
            .child(div().flex_1())
            .child(button(
                "title-find-in-files",
                IconName::FindInFiles,
                tr("Find in Files"),
                project_search::Toggle.boxed_clone(),
            ))
            .child(button(
                "title-commands",
                IconName::Command,
                tr("Command Palette"),
                command_palette::Toggle.boxed_clone(),
            ))
    }

    /// Status bar on the window frame: message on the left; position, cursors, language, and line
    /// endings on the right. Status bar on the window frame: on the left, a message or the path of
    /// the active file; on the right, the position, cursors, language as a colored badge (the file
    /// type color), and line endings.
    /// The bell at the right end of the status bar: the unread count (red with unread errors); a
    /// click shows or hides the Notifications window.
    fn render_bell(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let center = self.notification_center.read(cx);
        let unread = center.unread_count();
        let color = if center.unread_kind() == Some(notifications::NotificationKind::Error) {
            ui.error
        } else {
            ui.accent_text
        };
        let open = self.right_tool == Some(RightTool::Notifications);
        div().flex_none().pr(px(GAP + 2.)).child(
            div()
                .id("status-bell")
                .h(px(20.))
                .px_1()
                .flex()
                .items_center()
                .gap_1()
                .rounded(px(RADIUS_SM))
                .cursor_pointer()
                .when(open, |bell| bell.bg(ui.accent_soft))
                .when(!open, |bell| bell.hover(move |style| style.bg(ui.hover)))
                .tooltip(ui::tooltip(tr("Notifications"), None))
                .on_click(|_, window, cx| {
                    window.dispatch_action(Box::new(notifications_panel::Toggle), cx)
                })
                .child(
                    icon(
                        IconName::Bell,
                        if open {
                            ui.accent_text
                        } else if unread > 0 {
                            color
                        } else {
                            ui.text_muted
                        },
                    )
                    .size(px(13.)),
                )
                .when(unread > 0, |bell| {
                    bell.child(
                        div()
                            .text_size(px(theme::TEXT_XS))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(color)
                            .child(if unread > 99 {
                                "99+".to_string()
                            } else {
                                unread.to_string()
                            }),
                    )
                }),
        )
    }

    fn render_status_bar(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let bar = div()
            .flex_none()
            .h(px(STATUS_BAR_HEIGHT))
            .px(px(GAP + 6.))
            .flex()
            .items_center()
            .gap_3()
            .text_size(px(theme::TEXT_SM))
            .text_color(ui.text_muted);
        let editor = match self.active_item() {
            Some(TabItem::Editor(editor)) => editor,
            Some(TabItem::Terminal(group)) => return self.terminal_status(bar, &group, cx),
            Some(TabItem::Diff(view)) => match view.read(cx).working_copy().cloned() {
                Some(editor) => editor,
                None => {
                    let path = self.display_path(view.read(cx).path());
                    return bar.child(div().flex_1().min_w_0().truncate().child(path));
                }
            },
            Some(TabItem::Log(_)) => {
                let left: SharedString = self.status_message.clone().unwrap_or_default();
                return bar
                    .child(div().flex_1().min_w_0().truncate().child(left))
                    .children(crate::git::status_item(self.git.read(cx), ui));
            }
            Some(TabItem::Merge(view)) => {
                let path = self.display_path(view.read(cx).path());
                let left = match &self.status_message {
                    Some(message) => message.clone(),
                    None => path.into(),
                };
                return bar
                    .child(div().flex_1().min_w_0().truncate().child(left))
                    .children(crate::git::status_item(self.git.read(cx), ui));
            }
            None => {
                return bar.child(div().text_color(ui.dim).child(match &self.root {
                    Some(root) => tilde(root),
                    None => tr("No project — open a folder with ⌘O").into(),
                }));
            }
        };
        let editor_id = editor.entity_id();
        let editor = editor.read(cx);
        let status = editor.status_info();
        let path = editor.document.path();
        let file = file_icon(&editor.document.display_name(), &ui);
        let left = match status.message {
            Some(message) => div()
                .flex()
                .items_center()
                .gap_1p5()
                .min_w_0()
                .text_color(ui.foreground)
                .child(icon(IconName::Info, ui.info).size(px(13.)))
                .child(div().truncate().child(message)),
            None => div()
                .flex()
                .items_center()
                .gap_1p5()
                .min_w_0()
                .child(file.render().size(px(13.)))
                .child(div().truncate().child(match path {
                    Some(path) => self.display_path(path),
                    None => tr("Untitled").into(),
                })),
        };
        let item = |text: String| div().flex_none().whitespace_nowrap().child(text);
        bar.child(div().flex_1().min_w_0().flex().child(left))
            .child(item(trf(
                "Ln {0}, Col {1}",
                &[&status.line, &status.column],
            )))
            .when(status.cursors > 1, |bar| {
                bar.child(ui::badge(
                    trn(status.cursors, "{n} cursor", "{n} cursors"),
                    ui.accent_text,
                ))
            })
            .child(
                div()
                    .flex_none()
                    .h(px(20.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .rounded(px(10.))
                    .bg(UiColors::tint(file.color, 0.12))
                    .text_color(file.color)
                    .child(div().size(px(6.)).rounded(px(3.)).bg(file.color))
                    .child(item(status.language)),
            )
            .children(crate::diagnostics::status_item(editor, ui))
            .children(crate::git::status_item(self.git.read(cx), ui))
            .children(crate::lsp::status_item(
                self.lsp.read(cx),
                Some(editor_id),
                ui,
            ))
            .children(plugins::status_bar_items(self, cx))
            .child(item(status.line_ending.to_string()).text_color(ui.dim))
    }

    /// The status bar under a terminal tab: a message for a few seconds, otherwise the active
    /// terminal's process and directory.
    fn terminal_status(
        &self,
        bar: gpui::Div,
        group: &Entity<TerminalGroup>,
        cx: &Context<Self>,
    ) -> gpui::Div {
        let ui = Theme::ui(cx);
        if let Some(message) = &self.status_message {
            return bar.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .min_w_0()
                    .text_color(ui.foreground)
                    .child(icon(IconName::Info, ui.info).size(px(13.)))
                    .child(div().truncate().child(message.clone())),
            );
        }
        let view = group.read(cx).active_view();
        let view = view.read(cx);
        let directory = view.cwd.as_deref().map(tilde);
        bar.child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap_1p5()
                .child(icon(IconName::Terminal, ui.green).size(px(13.)))
                .child(
                    div()
                        .flex_none()
                        .text_color(ui.foreground)
                        .child(view.label()),
                )
                .children(directory.map(|directory| div().truncate().child(directory))),
        )
        .children(plugins::status_bar_items(self, cx))
    }

    /// Display path: relative to the project root, or with `~` outside it.
    fn display_path(&self, path: &Path) -> String {
        match self
            .root
            .as_deref()
            .and_then(|root| path.strip_prefix(root).ok())
        {
            Some(relative) => relative.display().to_string(),
            None => tilde(path),
        }
    }

    fn render_tab_bar(&self, cx: &Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let details = self.tab_details(cx);
        let tabs: Vec<_> = self
            .tabs
            .iter()
            .zip(details)
            .enumerate()
            .map(|(index, (tab, detail))| match &tab.item {
                TabItem::Editor(editor) => self
                    .render_editor_tab(index, editor, detail, cx)
                    .into_any_element(),
                TabItem::Terminal(group) => self
                    .render_terminal_tab(index, group, detail, cx)
                    .into_any_element(),
                TabItem::Diff(view) => self.render_diff_tab(index, view, cx).into_any_element(),
                TabItem::Merge(view) => self.render_merge_tab(index, view, cx).into_any_element(),
                TabItem::Log(view) => self.render_log_tab(index, view, cx).into_any_element(),
            })
            .collect();
        // While a tab is dragged over the strip: a marker where it would land.
        let marker = self.tab_drop.and_then(|index| {
            let strip = self.tab_scroll.bounds();
            let x = match self.tab_scroll.bounds_for_item(index) {
                Some(tab) => tab.left() - px(3.),
                None => {
                    self.tab_scroll
                        .bounds_for_item(index.checked_sub(1)?)?
                        .right()
                        + px(1.)
                }
            };
            Some(
                div()
                    .absolute()
                    .top(px(6.))
                    .bottom(px(6.))
                    .left(x - strip.left())
                    .w(px(2.))
                    .rounded(px(1.))
                    .bg(ui.accent),
            )
        });
        div()
            .relative()
            .flex_none()
            .h(px(TAB_BAR_HEIGHT))
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<DraggedTerminal>, _, cx| {
                    this.drag_over_tabs(event.event.position, event.bounds, cx)
                }),
            )
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<DraggedEditorTab>, _, cx| {
                    this.drag_over_tabs(event.event.position, event.bounds, cx)
                }),
            )
            .on_drop(cx.listener(|this, dragged: &DraggedTerminal, window, cx| {
                this.drop_terminal(dragged.group.clone(), window, cx)
            }))
            .on_drop(cx.listener(|this, dragged: &DraggedEditorTab, window, cx| {
                this.drop_editor_tab(dragged.editor.clone(), window, cx)
            }))
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<DraggedLogTab>, _, cx| {
                    this.drag_over_tabs(event.event.position, event.bounds, cx)
                }),
            )
            .on_drop(cx.listener(|this, dragged: &DraggedLogTab, window, cx| {
                this.drop_log(dragged.view.clone(), window, cx)
            }))
            // The line under the tabs runs from edge to edge of the island, inset from the rounded
            // corners.
            .child(
                div()
                    .absolute()
                    .left(px(GAP))
                    .right(px(GAP))
                    .bottom_0()
                    .h(px(1.))
                    .bg(ui.divider),
            )
            .child(
                div()
                    .id("tabs")
                    .size_full()
                    .px_1p5()
                    .flex()
                    .items_center()
                    .gap_1()
                    .overflow_x_scroll()
                    .track_scroll(&self.tab_scroll)
                    .children(tabs),
            )
            .children(marker)
    }

    /// The dimmed label after a tab's name: for files with the same name, their directories; for
    /// terminal tabs with the same process, their working directories.
    fn tab_details(&self, cx: &App) -> Vec<Option<String>> {
        let paths: Vec<Option<PathBuf>> = self
            .tabs
            .iter()
            .map(|tab| {
                let editor = tab.item.editor()?;
                editor.read(cx).document.path().map(Path::to_path_buf)
            })
            .collect();
        let terminals: Vec<Option<(SharedString, Option<PathBuf>)>> = self
            .tabs
            .iter()
            .map(|tab| {
                let group = tab.item.terminal()?.read(cx);
                let view = group.active_view();
                Some((group.title(cx), view.read(cx).cwd.clone()))
            })
            .collect();
        tab_details(&paths)
            .into_iter()
            .zip(terminal_details(&terminals))
            .map(|(file, terminal)| file.or(terminal))
            .collect()
    }

    fn render_editor_tab(
        &self,
        index: usize,
        editor: &Entity<Editor>,
        detail: Option<String>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let document = &editor.read(cx).document;
        let active = index == self.active;
        let (activate, close) = (editor.clone(), editor.clone());
        let name = document.display_name();
        let title = document_name(document);
        let file = file_icon(&name, &ui);
        let dragged = DraggedEditorTab {
            editor: editor.clone(),
            name: title.clone().into(),
        };
        let dot = document.is_modified().then_some(ui.modified);
        let status_color = document
            .path()
            .and_then(|path| crate::git::file_color(&self.git, path, &ui, cx));
        tab_shell(editor.entity_id(), active, ui)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    this.activate_editor(&activate, window, cx)
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(move |this, _: &MouseUpEvent, window, cx| {
                    this.close_tab(close.clone(), window, cx)
                }),
            )
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .child(file.render().size(px(14.)))
            .child(match status_color {
                Some(color) => label(&title).text_color(color),
                None => label(&title),
            })
            .children(detail.map(|detail| label(&detail).text_color(ui.dim)))
            .child(close_button(
                TabItem::Editor(editor.clone()),
                active,
                dot,
                cx,
            ))
    }

    /// A diff tab: the diff icon, the file name, "diff"; a dot while the working copy it opened
    /// without a tab has unsaved changes.
    fn render_diff_tab(
        &self,
        index: usize,
        view: &Entity<DiffView>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let active = index == self.active;
        let title = view.read(cx).title();
        let dot = TabItem::Diff(view.clone())
            .is_modified(cx)
            .then_some(ui.modified);
        let (activate, close) = (view.clone(), view.clone());
        tab_shell(view.entity_id(), active, ui)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    let index = this
                        .tabs
                        .iter()
                        .position(|tab| tab.item.diff() == Some(&activate));
                    if let Some(index) = index {
                        this.activate(index, window, cx);
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(move |this, _: &MouseUpEvent, window, cx| {
                    this.close_diff_tab(close.clone(), window, cx)
                }),
            )
            .child(icon(IconName::Diff, ui.vcs_modified).size(px(14.)))
            .child(label(&title))
            .child(label(tr("diff")).text_color(ui.dim))
            .child(close_button(TabItem::Diff(view.clone()), active, dot, cx))
    }

    /// A tab of the Git window in the editor area: the log or a history; it can be dragged back.
    fn render_log_tab(
        &self,
        index: usize,
        view: &Entity<GitLogView>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let active = index == self.active;
        let title = view.read(cx).title();
        let is_log = *view.read(cx).scope() == crate::git_log::LogScope::All;
        let (activate, close) = (view.clone(), view.clone());
        let dragged = DraggedLogTab {
            view: view.clone(),
            title: title.clone(),
        };
        tab_shell(view.entity_id(), active, ui)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    if let Some(index) = this.index_of_log(&activate) {
                        this.activate(index, window, cx);
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(move |this, _: &MouseUpEvent, window, cx| {
                    this.close_item(TabItem::Log(close.clone()), window, cx)
                }),
            )
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .child(
                icon(
                    if is_log {
                        IconName::Commit
                    } else {
                        IconName::History
                    },
                    ui.accent,
                )
                .size(px(14.)),
            )
            .child(label(&title))
            .child(close_button(TabItem::Log(view.clone()), active, None, cx))
    }

    /// The merge tool's tab: the file name, "merge", a dot while the result isn't applied.
    fn render_merge_tab(
        &self,
        index: usize,
        view: &Entity<MergeView>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let active = index == self.active;
        let title = view.read(cx).title();
        let dot = view.read(cx).is_modified(cx).then_some(ui.modified);
        let (activate, close) = (view.clone(), view.clone());
        tab_shell(view.entity_id(), active, ui)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    let index = this
                        .tabs
                        .iter()
                        .position(|tab| tab.item.merge() == Some(&activate));
                    if let Some(index) = index {
                        this.activate(index, window, cx);
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(move |this, _: &MouseUpEvent, window, cx| {
                    this.close_merge_tab(close.clone(), window, cx)
                }),
            )
            .child(icon(IconName::Merge, ui.vcs_conflict).size(px(14.)))
            .child(label(&title))
            .child(label(tr("merge")).text_color(ui.dim))
            .child(close_button(TabItem::Merge(view.clone()), active, dot, cx))
    }

    /// A terminal tab: the process of its active terminal, a mark when the bell rang while it was
    /// in the background.
    fn render_terminal_tab(
        &self,
        index: usize,
        group: &Entity<TerminalGroup>,
        detail: Option<String>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let active = index == self.active;
        let title = group.read(cx).title(cx);
        let dot = (!active && group.read(cx).has_bell()).then_some(ui.warning);
        let (activate, close) = (group.clone(), group.clone());
        let dragged = DraggedTerminal {
            group: group.clone(),
            title: title.clone(),
        };
        tab_shell(group.entity_id(), active, ui)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    if let Some(index) = this.index_of_terminal(&activate) {
                        this.activate(index, window, cx);
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(move |this, _: &MouseUpEvent, window, cx| {
                    this.close_terminal_tab(close.clone(), window, cx)
                }),
            )
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .child(icon(IconName::Terminal, ui.green).size(px(14.)))
            .child(label(&title))
            .children(detail.map(|detail| label(&detail).text_color(ui.dim)))
            .child(close_button(
                TabItem::Terminal(group.clone()),
                active,
                dot,
                cx,
            ))
    }

    // --- Dragging tabs ---

    /// A tab is dragged over the tab strip: the marker follows the insertion point; outside the
    /// strip, it goes away.
    fn drag_over_tabs(
        &mut self,
        position: Point<Pixels>,
        bounds: Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let index = bounds.contains(&position).then(|| {
            let centers: Vec<Pixels> = (0..self.tabs.len())
                .filter_map(|index| self.tab_scroll.bounds_for_item(index))
                .map(|tab| tab.center().x)
                .collect();
            insertion_index(&centers, position.x)
        });
        if index != self.tab_drop {
            self.tab_drop = index;
            cx.notify();
        }
    }

    /// A terminal tab dropped on the tab strip: one of its own tabs moves along the strip; one from
    /// the panel joins the tabs where the marker shows.
    fn drop_terminal(
        &mut self,
        group: Entity<TerminalGroup>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let index = self.tab_drop.take().unwrap_or(self.tabs.len());
        match self.index_of_terminal(&group) {
            Some(from) => self.move_tab(from, index, window, cx),
            None => self.move_terminal_to_editor(group, Some(index), window, cx),
        }
    }

    fn drop_editor_tab(
        &mut self,
        editor: Entity<Editor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let index = self.tab_drop.take().unwrap_or(self.tabs.len());
        if let Some(from) = self.index_of(&editor) {
            self.move_tab(from, index, window, cx);
        }
    }

    /// Moves a tab to an insertion point (an index between the tabs before the move) and activates
    /// it.
    fn move_tab(&mut self, from: usize, to: usize, window: &mut Window, cx: &mut Context<Self>) {
        let to = moved_index(from, to);
        let tab = self.tabs.remove(from);
        let to = to.min(self.tabs.len());
        self.tabs.insert(to, tab);
        self.activate(to, window, cx);
    }

    fn render_start(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Shortcut hints are looked up along the focus path of the last drawn frame. When the start
        // screen appears (the last tab closed), that frame didn't have it, and the hints would stay
        // empty until something else redraws the window: draw it once more. Callbacks for the next
        // frame run before that frame is drawn, so this is scheduled from the drawing itself.
        if !self.start_drawn {
            self.start_drawn = true;
            cx.on_next_frame(window, |_, _, cx| cx.notify());
        }
        let branch = self.git.read(cx).branch_label(None);
        let screen = StartScreen {
            root: self.root.as_deref(),
            branch: branch.as_ref().map(|branch| branch.as_ref()),
            recent: &self.recent,
            notice: self.notice.as_ref(),
            loading: self.loading > 0,
        };
        div()
            .track_focus(&self.focus_handle)
            .flex_1()
            .min_h_0()
            .child(start_screen::render(screen, window, cx))
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.update_title(window, cx);
        let ui = Theme::ui(cx);
        let root = div()
            .key_context("Workspace")
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(ui.frame)
            .text_color(ui.foreground)
            .font_family(theme::UI_FONT)
            .text_size(px(theme::TEXT_MD))
            .on_action(cx.listener(Self::open))
            // Language server navigation and refactoring.
            .map(|root| crate::navigation::actions(root, cx))
            .on_action(
                cx.listener(|this, _: &crate::settings_view::Toggle, window, cx| {
                    crate::settings_view::toggle(this, window, cx)
                }),
            )
            .map(|root| crate::lsp::workspace_actions(root, cx))
            .map(|root| crate::git::workspace_actions(root, cx))
            .map(|root| crate::vcs_menu::workspace_actions(root, cx))
            .map(|root| crate::branches_popup::workspace_actions(root, cx))
            .map(|root| crate::branch_dialogs::workspace_actions(root, cx))
            .map(|root| crate::git_sync::workspace_actions(root, cx))
            .map(|root| crate::compare_dialog::workspace_actions(root, cx))
            .map(|root| crate::stash_panel::workspace_actions(root, cx))
            .map(|root| crate::conflicts_dialog::workspace_actions(root, cx))
            .map(|root| crate::log_actions::workspace_actions(root, cx))
            .map(|root| crate::rebase_dialog::workspace_actions(root, cx))
            .map(|root| crate::blame::workspace_actions(root, cx))
            .map(|root| crate::plugins::workspace_actions(root, cx))
            .on_action(cx.listener(|this, action: &OpenProject, window, cx| {
                // A directory from the recent list may have disappeared since launch.
                if !action.0.is_dir() {
                    let message = trf("Folder not found: {0}", &[&tilde(&action.0)]);
                    let notification = Notification::warning(message);
                    return this.notify(notification, cx);
                }
                this.set_root(action.0.clone(), window, cx)
            }))
            .on_action(cx.listener(|this, _: &NewFile, window, cx| {
                this.add_document(Document::from_text(""), window, cx)
            }))
            // Esc in the editor: an open find bar is closed before the editor clears its selection;
            // when there is nothing to clear (the editor passes Esc on), project search is closed.
            .capture_action(cx.listener(|this, _: &editor::Cancel, window, cx| {
                if this.find_bar.read(cx).is_open() {
                    this.find_bar.update(cx, |bar, cx| bar.close(window, cx));
                    cx.stop_propagation();
                }
            }))
            .on_action(cx.listener(|this, _: &editor::Cancel, _, cx| {
                if this.project_search.read(cx).is_open() {
                    this.project_search
                        .update(cx, |search, cx| search.close(cx));
                }
            }))
            .on_action(cx.listener(|this, _: &editor::Save, _, cx| {
                if let Some(editor) = this.active_editor() {
                    editor.update(cx, |editor, cx| editor.save(cx).detach());
                }
            }))
            .on_action(cx.listener(Self::close_active_tab))
            .on_action(cx.listener(Self::close_window))
            .on_action(cx.listener(|this, _: &NextTab, window, cx| this.cycle(1, window, cx)))
            .on_action(cx.listener(|this, _: &PrevTab, window, cx| this.cycle(-1, window, cx)))
            .on_action(cx.listener(|this, action: &ActivateTab, window, cx| {
                this.activate(action.0, window, cx)
            }))
            .on_action(cx.listener(|this, _: &LastTab, window, cx| {
                this.activate(this.tabs.len().saturating_sub(1), window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &command_palette::Toggle, window, cx| {
                    command_palette::toggle(this, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &file_finder::Toggle, window, cx| {
                file_finder::toggle(this, window, cx)
            }))
            .on_action(cx.listener(|this, _: &go_to_line::Toggle, window, cx| {
                go_to_line::toggle(this, window, cx)
            }))
            .on_action(cx.listener(|this, _: &find_bar::Deploy, window, cx| {
                this.deploy_find(false, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &find_bar::DeployReplace, window, cx| {
                    this.deploy_find(true, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &find_bar::FindNext, window, cx| {
                this.find_bar
                    .update(cx, |bar, cx| bar.select_next(false, window, cx))
            }))
            .on_action(cx.listener(|this, _: &find_bar::FindPrevious, window, cx| {
                this.find_bar
                    .update(cx, |bar, cx| bar.select_next(true, window, cx))
            }))
            .on_action(cx.listener(|this, _: &file_tree::ToggleOpen, window, cx| {
                this.toggle_tree(window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &crate::git::ToggleGitWindow, window, cx| {
                    this.toggle_git_window(window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, action: &crate::git::ShowCommitInLog, window, cx| {
                    this.show_commit_in_log(action.repo, &action.oid, window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, action: &crate::git::ShowHistory, window, cx| {
                    this.show_history(&action.path, action.lines, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &git_window::HideWindow, window, cx| {
                this.hide_git_window(window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &terminal_panel::TogglePanel, window, cx| {
                    this.toggle_terminal(window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &terminal_panel::NewTerminal, window, cx| {
                    this.new_terminal(window, cx)
                }),
            )
            // ⇧Esc hides the panel only from one of its terminals.
            .on_action(
                cx.listener(|this, _: &terminal_panel::HidePanel, window, cx| {
                    if this.terminal_panel.read(cx).contains_focus(window, cx) {
                        this.hide_terminal(window, cx)
                    }
                }),
            )
            .on_action(
                cx.listener(|this, _: &notifications_panel::Toggle, window, cx| {
                    this.toggle_notifications(window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, action: &plugins::ToggleToolWindow, window, cx| {
                    let key = this
                        .plugins
                        .read(cx)
                        .tool_windows()
                        .iter()
                        .find(|tool| tool.plugin == action.plugin && tool.id == action.window)
                        .map(|tool| tool.key);
                    if let Some(key) = key {
                        this.toggle_plugin_tool(key, window, cx)
                    }
                }),
            )
            .on_action(cx.listener(|this, _: &file_tree::ToggleFocus, window, cx| {
                this.toggle_tree_focus(window, cx)
            }))
            .on_action(cx.listener(|this, _: &project_search::Toggle, window, cx| {
                // Project search and other overlay windows are never open at the same time; behind
                // a dialog, it doesn't open.
                if this.dialog_open() {
                    return;
                }
                this.dismiss_modal(window, cx);
                let seed = this
                    .active_editor()
                    .and_then(|editor| editor.read(cx).search_seed());
                this.project_search
                    .update(cx, |search, cx| search.toggle(seed, window, cx))
            }));
        #[cfg(feature = "scenario")]
        let root = root.on_action(cx.listener(
            |this, _: &notifications_panel::demo::FillDemo, _, cx| {
                this.notification_center
                    .update(cx, notifications_panel::demo::fill)
            },
        ));
        // The window frame: the title bar, the launchpad and the islands (the tree on the left; on
        // the right, the tabs, the find bar, and the text, a terminal tab, or the start screen; under
        // them, the terminal panel), the status bar. On top: project search and overlay windows.
        if self.tab_drop.is_some() && !cx.has_active_drag() {
            self.tab_drop = None;
        }
        // The island clips what doesn't fit (the start screen above an open terminal panel);
        // popups are deferred and escape it.
        let main = ui::island(ui)
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .flex()
            .flex_col();
        // A terminal tab dragged from the panel can be dropped anywhere in the editor area; on the
        // tab strip, it lands where the marker shows.
        let drop_terminal = cx.listener(|this, dragged: &DraggedTerminal, window, cx| {
            // A tab of the editor area dropped back onto it stays where it is.
            if this.index_of_terminal(&dragged.group).is_none() {
                this.move_terminal_to_editor(dragged.group.clone(), None, window, cx)
            }
        });
        let drop_log = cx.listener(|this, dragged: &DraggedLogTab, window, cx| {
            if this.index_of_log(&dragged.view).is_none() {
                this.move_log_to_editor(dragged.view.clone(), None, window, cx)
            }
        });
        let body = move |content: AnyElement| {
            div()
                .flex_1()
                .min_h_0()
                .on_drop(drop_terminal)
                .on_drop(drop_log)
                .child(content)
        };
        let main = match self.active_item() {
            Some(item) => {
                self.start_drawn = false;
                self.retry_tab_scroll(window, cx);
                let content = match &item {
                    TabItem::Editor(editor) => editor.clone().into_any_element(),
                    TabItem::Terminal(group) => group.clone().into_any_element(),
                    TabItem::Diff(view) => view.clone().into_any_element(),
                    TabItem::Merge(view) => view.clone().into_any_element(),
                    TabItem::Log(view) => view.clone().into_any_element(),
                };
                main.child(self.render_tab_bar(cx))
                    .when(
                        item.editor().is_some() && self.find_bar.read(cx).is_open(),
                        |main| main.child(self.find_bar.clone()),
                    )
                    .child(body(
                        div()
                            .size_full()
                            .pt_1p5()
                            .pb_2()
                            // Only a terminal of the editor area can move to the panel: the command
                            // is offered there.
                            .when(item.log().is_some(), |area| {
                                area.on_action(cx.listener(
                                    |this, _: &git_window::MoveToPanel, window, cx| {
                                        if let Some(view) =
                                            this.active_item().and_then(|item| item.log().cloned())
                                        {
                                            this.move_log_to_panel(view, None, window, cx)
                                        }
                                    },
                                ))
                            })
                            .when(item.terminal().is_some(), |area| {
                                area.on_action(cx.listener(
                                    |this, _: &terminal_panel::MoveToPanel, window, cx| {
                                        this.move_to_panel(window, cx)
                                    },
                                ))
                            })
                            .child(content)
                            .into_any_element(),
                    ))
            }
            None => main.child(body(self.render_start(window, cx).into_any_element())),
        };
        // The left island: the commit window in place of the tree, or the tree.
        let left = match &self.commit_panel {
            Some(handle) if self.commit_open => Some(handle.panel.clone().into_any_element()),
            _ => self
                .tree_panel()
                .filter(|_| self.tree_open)
                .map(|panel| panel.into_any_element()),
        };
        let tree = left.map(|panel| ui::island(ui).flex_none().h_full().child(panel));
        // The editor island and, under it, the terminal panel; the tree stays full height. A panel
        // terminal can move to the editor area: the command is offered inside the panel.
        let git_island = self
            .git_window
            .as_ref()
            .filter(|_| self.git_open)
            .map(|handle| {
                ui::island(ui)
                    .flex_none()
                    .on_action(
                        cx.listener(|this, _: &git_window::MoveToEditor, window, cx| {
                            this.move_log_tab_to_editor(window, cx)
                        }),
                    )
                    .child(handle.window.clone())
            });
        let terminal = (self.terminal_open && git_island.is_none()).then(|| {
            ui::island(ui)
                .flex_none()
                .on_action(
                    cx.listener(|this, _: &terminal_panel::MoveToEditor, window, cx| {
                        this.move_to_editor(window, cx)
                    }),
                )
                .child(self.terminal_panel.clone())
        });
        let main = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .gap(px(GAP))
            .child(main)
            .children(terminal)
            .children(git_island);
        // The island on the right: a tool window (Notifications), full height like the tree.
        let right = self.right_tool.map(|tool| {
            let content = match tool {
                RightTool::Notifications => self.notifications_panel.clone().into_any_element(),
                RightTool::Plugin(key) => match self.plugin_tool_view(key, cx) {
                    Some(view) => view.into_any_element(),
                    None => div().into_any_element(),
                },
            };
            ui::island(ui)
                .flex_none()
                .h_full()
                .on_action(cx.listener(|this, _: &notifications_panel::Hide, window, cx| {
                    this.hide_right(window, cx)
                }))
                .on_action(
                    cx.listener(|this, _: &notifications_panel::FocusEditor, window, cx| {
                        this.focus_active(window, cx)
                    }),
                )
                .on_action(
                    cx.listener(|this, _: &notifications_panel::OpenSettings, window, cx| {
                        crate::settings_view::open(
                            this,
                            crate::settings_view::Section::Notifications,
                            window,
                            cx,
                        )
                    }),
                )
                .child(content)
        });
        // The cards in the corner, unless the journal is in sight (then they go there, as in
        // JetBrains).
        let cards = (self.right_tool != Some(RightTool::Notifications))
            .then(|| notifications::overlay(self.notification_center.read(cx).cards(), cx))
            .flatten();
        // On the left, the launchpad (the tool strip) on the frame, followed by the islands.
        root.child(ui::frame_glow(ui))
            .child(self.render_title_bar(window, cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .pl(px(2.))
                    .pr(px(GAP))
                    .flex()
                    .flex_row()
                    .gap(px(GAP))
                    .child(launchpad::render(self, window, cx))
                    .children(tree)
                    .child(main)
                    .children(right),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .child(div().flex_1().min_w_0().child(self.render_status_bar(cx)))
                    .child(self.render_bell(cx)),
            )
            .children(cards)
            .children(self.render_project_search(cx))
            .children(self.render_modal(cx))
    }
}

// --- Prompts about unsaved documents ---

async fn ask_each(
    this: &WeakEntity<Workspace>,
    editors: Vec<Entity<Editor>>,
    cx: &mut AsyncWindowContext,
) -> bool {
    for editor in editors {
        if !ask_to_save(this, editor, cx).await {
            return false;
        }
    }
    true
}

/// Save / Don't Save / Cancel for a single document; Save on a document with no path becomes "Save
/// As". `true` means the document was saved or it was allowed to be discarded.
async fn ask_to_save(
    this: &WeakEntity<Workspace>,
    editor: Entity<Editor>,
    cx: &mut AsyncWindowContext,
) -> bool {
    // By the time the queue reached the document, it may have been saved.
    let Ok(Some(name)) = editor.read_with(cx, |editor, _| {
        let document = &editor.document;
        document.is_modified().then(|| document_name(document))
    }) else {
        return true;
    };
    let shown = this.update_in(cx, |this, window, cx| {
        this.activate_editor(&editor, window, cx)
    });
    if shown.is_err() {
        return false;
    }
    let answer = Dialog::warning(trf("Save changes to {0}?", &[&name]))
        .message(tr("Your changes will be lost if you don’t save them."))
        .primary(tr("Save"))
        .danger(tr("Don’t Save"))
        .cancel(tr("Cancel"))
        .show_async(cx)
        .await;
    match answer {
        Some(0) => match editor.update(cx, |editor, cx| editor.save(cx)) {
            Ok(saving) => saving.await,
            Err(_) => false,
        },
        Some(1) => true,
        _ => false,
    }
}

/// Quitting is already in progress: a repeated cmd-q while dialogs are open does not start a second
/// review.
#[derive(Default)]
struct Quitting(bool);

impl Global for Quitting {}

/// Quit: the windows go through their unsaved documents in turn; declining in any of them cancels
/// the quit.
fn quit(_: &Quit, cx: &mut App) {
    let quitting = cx.default_global::<Quitting>();
    if quitting.0 {
        return;
    }
    quitting.0 = true;
    let windows: Vec<WindowHandle<Workspace>> = cx
        .windows()
        .into_iter()
        .filter_map(|window| window.downcast())
        .collect();
    cx.spawn(async move |cx| {
        let confirmed = confirm_windows(windows, cx).await;
        cx.update(|cx| {
            if confirmed {
                cx.quit();
            } else {
                cx.global_mut::<Quitting>().0 = false;
            }
        })
        .ok();
    })
    .detach();
}

async fn confirm_windows(windows: Vec<WindowHandle<Workspace>>, cx: &mut AsyncApp) -> bool {
    for handle in windows {
        let confirm = handle.update(cx, |workspace, window, cx| {
            workspace.confirm_close_all(window, cx)
        });
        // The window was closed in the meantime: there is nothing to ask about.
        let Ok(confirm) = confirm else {
            continue;
        };
        if !confirm.await {
            return false;
        }
    }
    true
}

// --- Miscellaneous ---

/// Whether focus is in one of the terminal tab's terminals.
fn contains_focus(group: &Entity<TerminalGroup>, window: &Window, cx: &App) -> bool {
    group
        .read(cx)
        .views()
        .iter()
        .any(|view| view.focus_handle(cx).contains_focused(window, cx))
}

/// Restores focus to where it was (if it was remembered).
fn restore_focus(focus: Option<FocusHandle>, window: &mut Window) {
    if let Some(focus) = focus {
        window.focus(&focus);
    }
}

fn is_modified(editor: &Entity<Editor>, cx: &App) -> bool {
    editor.read(cx).document.is_modified()
}

/// The working copy a diff opened without a tab of its own.
fn owned_editor(view: &Entity<DiffView>, cx: &App) -> Option<Entity<Editor>> {
    let view = view.read(cx);
    view.working().filter(|_| view.owns_working()).cloned()
}

/// Reads a file (on a background thread). A directory is not opened as a file: it can only be a
/// project root.
fn read_document(path: PathBuf) -> Result<Document, OpenError> {
    // The real path: the same file opened as `/tmp/x` and `/private/tmp/x` is one tab, and git (whose
    // working trees are canonical) recognizes it.
    let path = canonical(&path);
    if path.is_dir() {
        let reason = "is a directory".into();
        return Err(OpenError { path, reason });
    }
    Document::open(&path).map_err(|err| OpenError {
        path,
        reason: err.to_string(),
    })
}

/// Path for the "is this the same file" comparison: canonical, or, for a file that does not exist
/// yet, the canonical directory plus the name.
fn canonical(path: &Path) -> PathBuf {
    if let Ok(path) = fs::canonicalize(path) {
        return path;
    }
    match (
        path.parent().and_then(|dir| fs::canonicalize(dir).ok()),
        path.file_name(),
    ) {
        (Some(dir), Some(name)) => dir.join(name),
        _ => path.to_path_buf(),
    }
}

/// Display path: the home directory is shown as `~`.
pub(crate) fn tilde(path: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match home
        .as_deref()
        .and_then(|home| path.strip_prefix(home).ok())
    {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// The name of a document in the interface: its file name, «untitled» before it is saved.
fn document_name(document: &Document) -> String {
    match document.path() {
        Some(_) => document.display_name(),
        None => tr("untitled").to_string(),
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Directory labels for tabs with identical file names: as many trailing components of the
/// directory path as needed to tell the file apart from its namesakes.
fn tab_details(paths: &[Option<PathBuf>]) -> Vec<Option<String>> {
    let all: Vec<&Path> = paths.iter().flatten().map(PathBuf::as_path).collect();
    paths
        .iter()
        .map(|path| {
            let path = path.as_deref()?;
            let namesakes: Vec<&Path> = all
                .iter()
                .copied()
                .filter(|other| *other != path && other.file_name() == path.file_name())
                .collect();
            if namesakes.is_empty() {
                return None;
            }
            let depth = path.parent().map_or(0, |dir| dir.components().count());
            let distinct = |n: &usize| {
                namesakes
                    .iter()
                    .all(|o| dir_tail(o, *n) != dir_tail(path, *n))
            };
            let n = (1..=depth).find(distinct).unwrap_or(depth);
            Some(dir_tail(path, n).display().to_string())
        })
        .collect()
}

/// The last `n` components of the directory containing the file.
fn dir_tail(path: &Path, n: usize) -> PathBuf {
    let dir: Vec<_> = path
        .parent()
        .map(|dir| dir.components().collect())
        .unwrap_or_default();
    dir[dir.len().saturating_sub(n)..].iter().collect()
}

fn label(text: &str) -> gpui::Div {
    div()
        .whitespace_nowrap()
        .child(shorten(text, TAB_LABEL_MAX_CHARS))
}

/// Shortens in the middle: "start…end", so that both the beginning of the name and the extension
/// stay visible. The font is monospaced, so the character count is also the width. (gpui's own
/// ellipsis does not work here: in a scrollable strip the text width is unconstrained.)
fn shorten(text: &str, max_chars: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars {
        return text.to_string();
    }
    let head = (max_chars - 1) / 2;
    let tail = max_chars - 1 - head;
    let mut short: String = chars[..head].iter().collect();
    short.push('…');
    short.extend(&chars[chars.len() - tail..]);
    short
}

/// A tab: a rounded pill, highlighted when active.
fn tab_shell(id: EntityId, active: bool, ui: UiColors) -> gpui::Stateful<gpui::Div> {
    div()
        .id(("tab", id))
        .group("tab")
        .relative()
        .flex_none()
        .h(px(TAB_HEIGHT))
        .pl_2p5()
        .pr_1p5()
        .flex()
        .items_center()
        .gap_2()
        .rounded(px(RADIUS_MD))
        .border_1()
        .text_color(if active { ui.foreground } else { ui.text_muted })
        .when(active, |el| {
            el.bg(ui.pressed)
                .border_color(ui.island_border)
                .font_weight(FontWeight::MEDIUM)
        })
        .when(!active, |el| {
            el.border_color(gpui::transparent_black())
                .hover(|style| style.bg(ui.hover).text_color(ui.foreground))
        })
}

/// Right edge of a tab: a dot in `dot`'s color (unsaved changes, a terminal's bell), "×" on hover
/// (always on an active tab without a dot).
fn close_button(
    item: TabItem,
    active: bool,
    dot: Option<gpui::Hsla>,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let ui = Theme::ui(cx);
    let close = div()
        .id("close")
        .group("tab-close")
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(ui::RADIUS_XS))
        .text_color(ui.dim)
        .hover(move |style| style.bg(ui.pressed).text_color(ui.foreground))
        .when(dot.is_some() || !active, |close| {
            close
                .invisible()
                .group_hover("tab", |style| style.visible())
        })
        // Clicking "×" must not activate the tab.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            this.close_item(item.clone(), window, cx)
        }))
        .child(
            icon(IconName::Close, ui.dim)
                .size(px(12.))
                .group_hover("tab-close", move |style| style.text_color(ui.foreground)),
        );
    let dot = dot.map(|color| {
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .group_hover("tab", |style| style.invisible())
            .child(div().size(px(7.)).rounded(px(4.)).bg(color))
    });
    div()
        .relative()
        .flex_none()
        .size(px(18.))
        .children(dot)
        .child(close)
}

/// The insertion point for a tab dropped at `x`: the number of tabs whose middle is to its left.
fn insertion_index(centers: &[Pixels], x: Pixels) -> usize {
    centers.iter().filter(|center| **center < x).count()
}

/// Where a tab lands when moved from `from` to the insertion point `to` (counted before the move):
/// past its own slot, the points shift left by one.
fn moved_index(from: usize, to: usize) -> usize {
    if to > from { to - 1 } else { to }
}

/// Details for terminal tabs (`None` for other tabs): those with the same process name get their
/// working directory's name, so that "zsh" and "zsh" can be told apart.
fn terminal_details(terminals: &[Option<(SharedString, Option<PathBuf>)>]) -> Vec<Option<String>> {
    terminals
        .iter()
        .map(|terminal| {
            let (title, cwd) = terminal.as_ref()?;
            let namesakes = terminals
                .iter()
                .flatten()
                .filter(|(other, _)| other == title)
                .count();
            if namesakes < 2 {
                return None;
            }
            let cwd = cwd.as_deref()?;
            Some(match cwd.file_name() {
                Some(_) if tilde(cwd) == "~" => "~".to_string(),
                Some(name) => name.to_string_lossy().into_owned(),
                None => cwd.display().to_string(),
            })
        })
        .collect()
}

/// The detail of the "Terminate running processes?" question, naming the commands (each once, with
/// a count when it runs in several terminals); `None` when nothing is running.
fn running_processes_detail(names: &[String]) -> Option<String> {
    let mut counted: Vec<(&str, usize)> = Vec::new();
    for name in names {
        match counted.iter_mut().find(|(seen, _)| seen == name) {
            Some((_, count)) => *count += 1,
            None => counted.push((name, 1)),
        }
    }
    match counted.as_slice() {
        [] => None,
        [(name, 1)] => Some(trf("“{0}” is still running in a terminal.", &[name])),
        _ => {
            let list = counted
                .iter()
                .map(|(name, count)| match count {
                    1 => name.to_string(),
                    count => format!("{name} ×{count}"),
                })
                .collect::<Vec<_>>()
                .join(", ");
            Some(trf("{0} are still running in terminals.", &[&list]))
        }
    }
}

impl Render for DraggedEditorTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let file = file_icon(&self.name, &ui);
        div()
            .flex()
            .items_center()
            .gap_2()
            .h(px(TAB_HEIGHT))
            .pl_2p5()
            .pr_3()
            .rounded(px(RADIUS_MD))
            // Opaque: the label floats over the tabs, and their text must not show through.
            .bg(UiColors::tint(ui.elevated, 1.))
            .border_1()
            .border_color(ui.elevated_border)
            .shadow(ui::popover_shadow(ui))
            .font_family(theme::UI_FONT)
            .text_size(px(theme::TEXT_MD))
            .text_color(ui.foreground)
            .child(file.render().size(px(14.)))
            .child(self.name.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn details(paths: &[&str]) -> Vec<Option<String>> {
        let paths: Vec<Option<PathBuf>> = paths.iter().map(|p| Some(PathBuf::from(p))).collect();
        tab_details(&paths)
    }

    #[test]
    fn unique_names_have_no_detail() {
        assert_eq!(details(&["/a/main.rs", "/a/lib.rs"]), vec![None, None]);
    }

    #[test]
    fn namesakes_get_parent_dir() {
        assert_eq!(
            details(&["/x/core/lib.rs", "/x/app/lib.rs", "/x/app/main.rs"]),
            vec![Some("core".into()), Some("app".into()), None]
        );
    }

    #[test]
    fn same_parent_name_goes_deeper() {
        assert_eq!(
            details(&["/x/core/src/lib.rs", "/x/app/src/lib.rs"]),
            vec![Some("core/src".into()), Some("app/src".into())]
        );
    }

    #[test]
    fn long_labels_are_shortened_in_the_middle() {
        assert_eq!(shorten("main.rs", 9), "main.rs");
        assert_eq!(shorten("abcdefghij.md", 9), "abcd…j.md");
        assert_eq!(shorten("абвгдеёжзий", 5), "аб…ий");
    }

    #[test]
    fn home_is_shown_as_tilde() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        assert_eq!(tilde(&home), "~");
        assert_eq!(tilde(&home.join("dev/flux")), "~/dev/flux");
        assert_eq!(tilde(Path::new("/opt/x")), "/opt/x");
    }

    #[test]
    fn dropped_tabs_land_between_the_tabs_under_the_pointer() {
        let centers = [px(50.), px(150.), px(250.)];
        assert_eq!(insertion_index(&centers, px(10.)), 0);
        assert_eq!(insertion_index(&centers, px(120.)), 1);
        assert_eq!(insertion_index(&centers, px(400.)), 3);
        // A tab moved to the right skips its own slot; to the left, it lands right there.
        assert_eq!(moved_index(0, 2), 1);
        assert_eq!(moved_index(0, 3), 2);
        assert_eq!(moved_index(2, 0), 0);
        assert_eq!(moved_index(1, 1), 1);
    }

    #[test]
    fn terminal_tabs_with_the_same_process_show_their_directory() {
        let zsh = |dir: &str| Some((SharedString::from("zsh"), Some(PathBuf::from(dir))));
        let terminals = vec![
            zsh("/x/flux"),
            None,
            zsh("/x/crates"),
            Some(("cargo".into(), Some(PathBuf::from("/x/flux")))),
        ];
        assert_eq!(
            terminal_details(&terminals),
            vec![Some("flux".into()), None, Some("crates".into()), None]
        );
    }

    #[test]
    fn the_quit_question_names_the_running_commands_once() {
        assert_eq!(running_processes_detail(&[]), None);
        assert_eq!(
            running_processes_detail(&["cargo".into()]).as_deref(),
            Some("“cargo” is still running in a terminal.")
        );
        assert_eq!(
            running_processes_detail(&["cargo".into(), "vim".into(), "cargo".into()]).as_deref(),
            Some("cargo ×2, vim are still running in terminals.")
        );
        assert_eq!(
            running_processes_detail(&["cargo".into(), "cargo".into()]).as_deref(),
            Some("cargo ×2 are still running in terminals.")
        );
    }

    #[test]
    fn untitled_documents_are_ignored() {
        let paths = vec![None, Some(PathBuf::from("/a/lib.rs")), None];
        assert_eq!(tab_details(&paths), vec![None, None, None]);
    }
}
