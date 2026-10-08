//! Root view of the window: the file tree, tabs with documents, opening files, closing tabs and the
//! window, and quitting, with prompts about unsaved changes.

use std::fs;
use std::path::{Path, PathBuf};

use flux_core::Document;
use flux_fs::remap;
use gpui::{
    Action, AnyElement, AnyView, App, AsyncApp, AsyncWindowContext, ClickEvent, Context,
    DismissEvent, Entity, FocusHandle, Focusable, FontWeight, Global, KeyBinding, ManagedView,
    MouseButton, MouseDownEvent, MouseUpEvent, PathPromptOptions, Pixels, Point, PromptLevel,
    Render, ScrollHandle, SharedString, Subscription, Task, WeakEntity, Window, WindowHandle,
    actions, anchored, deferred, div, prelude::*, px, relative,
};

use crate::editor::{self, Editor};
use crate::file_tree::{self, FileTreeEvent, FileTreePanel};
use crate::find_bar::{self, FindBar};
use crate::i18n::{tr, trf, trn};
use crate::icons::{IconName, file_icon, icon};
use crate::launchpad::{self, Tool};
use crate::lsp::LspStore;
use crate::project_search::{self, ProjectSearch, ProjectSearchEvent};
use crate::start_screen::{self, StartScreen};
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

/// Root view of the window: the tabs (one editor per document) and the active one among them, the
/// search panels, the overlay windows.
pub struct Workspace {
    /// Project root: files (cmd-p) and text (cmd-shift-f) are searched within it.
    root: Option<PathBuf>,
    tabs: Vec<Tab>,
    active: usize,
    /// Focus handle of the empty window: without it, cmd-o, cmd-n, cmd-w, and cmd-q would not work.
    focus_handle: FocusHandle,
    tab_scroll: ScrollHandle,
    /// Unsaved documents are being reviewed: new close requests are ignored.
    closing: bool,
    /// How many batches of files are still being read in the background.
    loading: usize,
    /// Message in the empty window; when tabs are open, messages go to the editor's status bar.
    notice: Option<SharedString>,
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
    /// The git branch of the project root, shown in the title bar; re-read when the window is
    /// activated.
    branch: Option<SharedString>,
    /// Recent projects for the start screen, most recent first.
    recent: Vec<PathBuf>,
    /// Language servers of the project; recreated with the root.
    pub(crate) lsp: Entity<LspStore>,
    _subscriptions: Vec<Subscription>,
}

/// The file tree and the subscription to its events; recreated whenever the project root changes.
struct TreePanel {
    panel: Entity<FileTreePanel>,
    _subscription: Subscription,
}

struct Tab {
    editor: Entity<Editor>,
    /// The tab strip and the window title are redrawn when the editor changes.
    _observer: Subscription,
}

/// Overlay window. It closes by itself (`DismissEvent`: Esc, making a choice), on a click outside
/// it, when the same window is invoked again, or when focus has left it (for example, cmd-n opened
/// a tab); focus returns to where it was before opening.
struct Modal {
    view: AnyView,
    focus_handle: FocusHandle,
    previous_focus: Option<FocusHandle>,
    /// Shown at this window point (rename, under its symbol) instead of the top center.
    anchor: Option<Point<Pixels>>,
    _subscriptions: [Subscription; 2],
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
        let file_tree = root.clone().map(|root| Self::build_tree(root, window, cx));
        let lsp = Self::build_lsp(root.clone(), cx);
        // The panels open and close on their own (Esc, ×); when they do, the window layout changes
        // too.
        let subscriptions = vec![
            // The branch may have been switched in a terminal while the window was inactive.
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.refresh_branch(cx);
                }
            }),
            cx.observe(&find_bar, |_, _, cx| cx.notify()),
            cx.observe(&project_search, |_, _, cx| cx.notify()),
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
        let branch = root.as_deref().and_then(read_branch);
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
            closing: false,
            loading: 0,
            notice: None,
            title: String::new(),
            edited: false,
            modal: None,
            navigation: Default::default(),
            find_bar,
            project_search,
            file_tree,
            tree_open: true,
            revealed: None,
            branch,
            recent,
            lsp,
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
        self.file_tree = Some(Self::build_tree(root.clone(), window, cx));
        self.revealed = None;
        self.branch = read_branch(&root);
        self.recent = recent::record(&root);
        self.show_message(trf("Project: {0}", &[&tilde(&root)]).into(), cx);
        self.root = Some(root.clone());
        self.lsp = Self::build_lsp(Some(root), cx);
        for tab in &self.tabs {
            self.lsp
                .update(cx, |store, cx| store.register(&tab.editor, cx));
        }
        self.reveal_active(cx);
        cx.notify();
    }

    /// Language servers for a project root; the status bar follows their status.
    fn build_lsp(root: Option<PathBuf>, cx: &mut Context<Self>) -> Entity<LspStore> {
        let lsp = cx.new(|cx| LspStore::new(root, cx));
        cx.observe(&lsp, |_, _, cx| cx.notify()).detach();
        lsp
    }

    /// Re-reads the git branch (`.git/HEAD`); redraws only if it changed.
    fn refresh_branch(&mut self, cx: &mut Context<Self>) {
        let branch = self.root.as_deref().and_then(read_branch);
        if branch != self.branch {
            self.branch = branch;
            cx.notify();
        }
    }

    pub(crate) fn editors(&self) -> Vec<Entity<Editor>> {
        self.tabs.iter().map(|tab| tab.editor.clone()).collect()
    }

    pub(crate) fn active_editor(&self) -> Option<Entity<Editor>> {
        self.tabs.get(self.active).map(|tab| tab.editor.clone())
    }

    fn index_of(&self, editor: &Entity<Editor>) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.editor == *editor)
    }

    // --- Tabs ---

    /// Makes the tab active and moves focus to its editor.
    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let editor = tab.editor.clone();
        window.focus(&editor.focus_handle(cx));
        self.active = index;
        self.tab_scroll.scroll_to_item(index);
        self.find_bar.update(cx, |bar, cx| {
            bar.set_active_editor(Some(editor), window, cx)
        });
        self.reveal_active(cx);
        cx.notify();
    }

    /// Focuses the active editor, or the window itself when there are no tabs.
    fn focus_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.active_editor() {
            Some(editor) => window.focus(&editor.focus_handle(cx)),
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
        let editor = cx.new(|cx| Editor::new(document, window, cx));
        self.lsp
            .update(cx, |store, cx| store.register(&editor, cx));
        // A document's path changes on "Save As": the tree then shows the new file.
        let observer = cx.observe(&editor, |this, _, cx| {
            this.reveal_active(cx);
            cx.notify()
        });
        let index = if self.tabs.is_empty() {
            0
        } else {
            self.active + 1
        };
        self.tabs.insert(
            index,
            Tab {
                editor,
                _observer: observer,
            },
        );
        self.notice = None;
        self.activate(index, window, cx);
    }

    /// Removes a tab. If it was active, focus moves to a neighboring one (the right one; for the
    /// last tab, the left one); with no tabs left, to the window itself.
    fn remove_tab(&mut self, editor: &Entity<Editor>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.index_of(editor) else {
            return;
        };
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.active = 0;
            window.focus(&self.focus_handle);
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
            let document = &tab.editor.read(cx).document;
            document
                .path()
                .is_some_and(|path| canonical(path) == target)
        });
        if let Some(index) = found {
            self.activate(index, window, cx);
        }
        found.is_some()
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
        let message = errors
            .iter()
            .map(|error| {
                trf(
                    "Cannot open {0}: {1}",
                    &[&file_name(&error.path), &tr(&error.reason)],
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        self.show_message(message.into(), cx);
    }

    /// A message to the user: in the active editor's status bar, or, in an empty window, under the
    /// hint.
    pub(crate) fn show_message(&mut self, message: SharedString, cx: &mut Context<Self>) {
        match self.active_editor() {
            Some(editor) => editor.update(cx, |editor, cx| editor.show_status(message, cx)),
            None => {
                self.notice = Some(message);
                cx.notify();
            }
        }
    }

    // --- Closing ---

    fn close_active_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        match self.active_editor() {
            Some(editor) => self.close_tab(editor, window, cx),
            // In an empty window, cmd-w closes the window itself.
            None => window.remove_window(),
        }
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

    /// `true` means the window can be closed right away. Otherwise asks about the unsaved documents
    /// and closes the window itself unless the user cancels.
    fn request_close_window(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.closing {
            return false;
        }
        if !self.tabs.iter().any(|tab| is_modified(&tab.editor, cx)) {
            return true;
        }
        let confirm = self.confirm(self.editors(), window, cx);
        cx.spawn_in(window, async move |_, cx| {
            if confirm.await {
                cx.update(|window, _| window.remove_window()).ok();
            }
        })
        .detach();
        false
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

    /// cmd-f / cmd-alt-f: the find bar for the active tab.
    fn deploy_find(&mut self, replace: bool, window: &mut Window, cx: &mut Context<Self>) {
        let editor = self.active_editor();
        self.find_bar
            .update(cx, |bar, cx| bar.deploy(replace, editor, window, cx));
    }

    // --- File tree ---

    fn build_tree(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> TreePanel {
        let panel = cx.new(|cx| FileTreePanel::new(root, window, cx));
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
            FileTreeEvent::Message(message) => self.show_message(message.clone(), cx),
        }
    }

    /// Whether a launchpad tool window is open: its button is highlighted.
    pub(crate) fn tool_open(&self, tool: Tool, cx: &App) -> bool {
        match tool {
            Tool::Project => self.tree_open && self.file_tree.is_some(),
            Tool::FindInFiles => self.project_search.read(cx).is_open(),
        }
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
        if !self.tree_open {
            self.tree_open = true;
            self.revealed = None;
            self.reveal_active(cx);
        }
        window.focus(&handle);
        cx.notify();
    }

    /// A file or directory was moved (renamed, or moved within the tree): the open documents inside
    /// it are switched to the new paths.
    fn documents_moved(&mut self, from: &Path, to: &Path, cx: &mut Context<Self>) {
        for editor in self.editors() {
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
        for editor in self.editors() {
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
            self.show_message(
                tr("Deleted files with unsaved changes stay open — save to restore them").into(),
                cx,
            );
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
        if let Some(modal) = &self.modal
            && modal.view.clone().downcast::<V>().is_ok()
        {
            return self.dismiss_modal(window, cx);
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
        let subscriptions = [
            cx.subscribe_in(&view, window, |this, _, _: &DismissEvent, window, cx| {
                this.dismiss_modal(window, cx)
            }),
            cx.on_focus_out(&focus_handle, window, |this, _, window, cx| {
                this.dismiss_modal(window, cx)
            }),
        ];
        window.focus(&focus_handle);
        self.modal = Some(Modal {
            view: view.into(),
            focus_handle,
            previous_focus,
            anchor: None,
            _subscriptions: subscriptions,
        });
        cx.notify();
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
            match (modal.previous_focus, self.active_editor()) {
                (Some(previous), _) => window.focus(&previous),
                (None, Some(editor)) => window.focus(&editor.focus_handle(cx)),
                (None, None) => window.focus(&self.focus_handle),
            }
        }
        cx.notify();
    }

    fn render_modal(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let modal = self.modal.as_ref()?;
        let view = div()
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.dismiss_modal(window, cx)))
            .child(modal.view.clone());
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

    /// The window title "● name — project", derived from the active tab (without a project: "—
    /// flux"), and a dot on the red button if there are unsaved changes. We call into the platform
    /// only when something has changed.
    fn update_title(&mut self, window: &mut Window, cx: &App) {
        let project = self.root.as_deref().and_then(Path::file_name).map_or_else(
            || "Flux".to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let title = self.active_editor().map_or_else(
            || project.clone(),
            |editor| {
                let document = &editor.read(cx).document;
                let modified = if document.is_modified() { "● " } else { "" };
                format!("{modified}{} — {project}", document_name(document))
            },
        );
        if title != self.title {
            window.set_window_title(&title);
            self.title = title;
        }
        let edited = self.tabs.iter().any(|tab| is_modified(&tab.editor, cx));
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
        let branch = self.branch.clone().map(|branch| {
            div()
                .flex()
                .items_center()
                .gap_1()
                .h(px(22.))
                .px_2()
                .rounded(px(11.))
                .bg(UiColors::tint(ui.violet, 0.12))
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.violet)
                .child(icon(IconName::Branch, ui.violet).size(px(12.)))
                .child(branch)
        });
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
        let Some(editor) = self.active_editor() else {
            return bar.child(div().text_color(ui.dim).child(match &self.root {
                Some(root) => tilde(root),
                None => tr("No project — open a folder with ⌘O").into(),
            }));
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
            .children(crate::lsp::status_item(self.lsp.read(cx), Some(editor_id), ui))
            .child(item(status.line_ending.to_string()).text_color(ui.dim))
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
        let paths: Vec<Option<PathBuf>> = self
            .tabs
            .iter()
            .map(|tab| tab.editor.read(cx).document.path().map(Path::to_path_buf))
            .collect();
        let tabs: Vec<_> = self
            .tabs
            .iter()
            .zip(tab_details(&paths))
            .enumerate()
            .map(|(index, (tab, detail))| self.render_tab(index, tab, detail, cx))
            .collect();
        div()
            .relative()
            .flex_none()
            .h(px(TAB_BAR_HEIGHT))
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
    }

    fn render_tab(
        &self,
        index: usize,
        tab: &Tab,
        detail: Option<String>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let document = &tab.editor.read(cx).document;
        let active = index == self.active;
        let (activate, close) = (tab.editor.clone(), tab.editor.clone());
        let name = document.display_name();
        let title = document_name(document);
        let file = file_icon(&name, &ui);
        div()
            .id(("tab", tab.editor.entity_id()))
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
            .child(file.render().size(px(14.)))
            .child(label(&title))
            .children(detail.map(|detail| label(&detail).text_color(ui.dim)))
            .child(close_button(
                tab.editor.clone(),
                active,
                document.is_modified(),
                cx,
            ))
    }

    fn render_start(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let screen = StartScreen {
            root: self.root.as_deref(),
            branch: self.branch.as_ref().map(|branch| branch.as_ref()),
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
            .on_action(cx.listener(|this, _: &crate::settings_view::Toggle, window, cx| {
                crate::settings_view::toggle(this, window, cx)
            }))
            .map(|root| crate::lsp::workspace_actions(root, cx))
            .on_action(cx.listener(|this, action: &OpenProject, window, cx| {
                // A directory from the recent list may have disappeared since launch.
                if !action.0.is_dir() {
                    let message = trf("Folder not found: {0}", &[&tilde(&action.0)]);
                    return this.show_message(message.into(), cx);
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
            .on_action(cx.listener(|this, _: &file_tree::ToggleFocus, window, cx| {
                this.toggle_tree_focus(window, cx)
            }))
            .on_action(cx.listener(|this, _: &project_search::Toggle, window, cx| {
                // Project search and other overlay windows are never open at the same time.
                this.dismiss_modal(window, cx);
                let seed = this
                    .active_editor()
                    .and_then(|editor| editor.read(cx).search_seed());
                this.project_search
                    .update(cx, |search, cx| search.toggle(seed, window, cx))
            }));
        // The window frame: the title bar, the launchpad and the islands (the tree on the left; on
        // the right, the tabs, the find bar, and the text or the start screen), the status bar. On
        // top: project search and overlay windows.
        let active = self.active_editor();
        let main = ui::island(ui).flex_1().min_w_0().h_full().flex().flex_col();
        let main = match &active {
            Some(editor) => {
                self.retry_tab_scroll(window, cx);
                main.child(self.render_tab_bar(cx))
                    .when(self.find_bar.read(cx).is_open(), |main| {
                        main.child(self.find_bar.clone())
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .pt_1p5()
                            .pb_2()
                            .child(editor.clone()),
                    )
            }
            None => main.child(self.render_start(window, cx)),
        };
        let tree = self
            .tree_panel()
            .filter(|_| self.tree_open)
            .map(|panel| ui::island(ui).flex_none().h_full().child(panel));
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
                    .child(main),
            )
            .child(self.render_status_bar(cx))
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
    let answer = cx.prompt(
        PromptLevel::Warning,
        &trf("Save changes to {0}?", &[&name]),
        Some(tr("Your changes will be lost if you don’t save them.")),
        &[tr("Save"), tr("Don’t Save"), tr("Cancel")],
    );
    match answer.await {
        Ok(0) => match editor.update(cx, |editor, cx| editor.save(cx)) {
            Ok(saving) => saving.await,
            Err(_) => false,
        },
        Ok(1) => true,
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
            workspace.confirm(workspace.editors(), window, cx)
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

/// Restores focus to where it was (if it was remembered).
fn restore_focus(focus: Option<FocusHandle>, window: &mut Window) {
    if let Some(focus) = focus {
        window.focus(&focus);
    }
}

fn is_modified(editor: &Entity<Editor>, cx: &App) -> bool {
    editor.read(cx).document.is_modified()
}

/// Reads a file (on a background thread). A directory is not opened as a file: it can only be a
/// project root.
fn read_document(path: PathBuf) -> Result<Document, OpenError> {
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

/// Right edge of a tab: "●" on a modified one, "×" on hover (always on an active unmodified tab).
fn close_button(
    editor: Entity<Editor>,
    active: bool,
    modified: bool,
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
        .when(modified || !active, |close| {
            close
                .invisible()
                .group_hover("tab", |style| style.visible())
        })
        // Clicking "×" must not activate the tab.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            this.close_tab(editor.clone(), window, cx)
        }))
        .child(
            icon(IconName::Close, ui.dim)
                .size(px(12.))
                .group_hover("tab-close", move |style| style.text_color(ui.foreground)),
        );
    let dot = modified.then(|| {
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .group_hover("tab", |style| style.invisible())
            .child(div().size(px(7.)).rounded(px(4.)).bg(ui.modified))
    });
    div()
        .relative()
        .flex_none()
        .size(px(18.))
        .children(dot)
        .child(close)
}

/// The git branch of a directory: `ref: refs/heads/main` → "main"; a detached HEAD gives the first
/// 7 characters of the hash. A linked worktree (`.git` is a file with `gitdir:`) is also read.
fn read_branch(root: &Path) -> Option<SharedString> {
    let git = root.join(".git");
    let dir = if git.is_file() {
        let link = fs::read_to_string(&git).ok()?;
        let target = link.strip_prefix("gitdir:")?.trim();
        root.join(target)
    } else {
        git
    };
    let head = fs::read_to_string(dir.join("HEAD")).ok()?;
    branch_from_head(&head)
}

fn branch_from_head(head: &str) -> Option<SharedString> {
    let head = head.trim();
    match head.strip_prefix("ref:") {
        Some(reference) => {
            let reference = reference.trim();
            let name = reference.strip_prefix("refs/heads/").unwrap_or(reference);
            Some(name.to_string().into())
        }
        None if head.len() >= 7 => Some(head[..7].to_string().into()),
        None => None,
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
    fn branch_comes_from_head() {
        assert_eq!(
            branch_from_head("ref: refs/heads/main\n")
                .as_ref()
                .map(|b| b.as_ref()),
            Some("main")
        );
        assert_eq!(
            branch_from_head("ref: refs/heads/feature/x")
                .as_ref()
                .map(|b| b.as_ref()),
            Some("feature/x")
        );
        assert_eq!(
            branch_from_head("4d7de13a9f00c0ffee\n")
                .as_ref()
                .map(|b| b.as_ref()),
            Some("4d7de13")
        );
        assert_eq!(branch_from_head(""), None);
    }

    #[test]
    fn untitled_documents_are_ignored() {
        let paths = vec![None, Some(PathBuf::from("/a/lib.rs")), None];
        assert_eq!(tab_details(&paths), vec![None, None, None]);
    }
}
