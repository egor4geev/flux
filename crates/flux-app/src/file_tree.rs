//! Project file tree: a panel to the left of the text (⌘1 shows/hides it, ⇧⌘E focuses it).
//!
//! State lives in `flux_fs::tree::FileTree`: which directories have been read and which are
//! expanded. Directories are read only in the background (`flux_fs::list_dir`): the root when the
//! panel is created, the others when they are expanded or when the tree navigates to the active
//! tab's file ([`FileTreePanel::reveal`]). The response to a stale read request (the directory was
//! requested again in the meantime) is discarded by request number. Changes on disk are reported by
//! `flux_fs::Watcher`: events are coalesced, and only the directories that have been read and are
//! affected by them are re-read.
//!
//! Operations — create, rename, move (⌘X ⌘V, drag and drop), copy, delete to the Trash — also run
//! in the background; the affected directories are re-read right after them, without waiting for
//! events. Workspace updates the open documents in response to [`FileTreeEvent`] events.

use std::collections::{BTreeSet, HashMap};
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Duration;

use flux_fs::tree::{FileTree, Row};
use flux_fs::{
    DirEntry, EntryKind, FsChange, Watcher, copy_into, create_dir, create_file, list_dir,
    move_into, remap, rename, trash, validate_name,
};
use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    Action, AnyElement, App, ClickEvent, ClipboardItem, Context, CursorStyle, DismissEvent, Div,
    DragMoveEvent, Entity, EventEmitter, FocusHandle, Focusable, KeyBinding, MouseButton,
    MouseDownEvent, Pixels, Point, Render, ScrollStrategy, SharedString, Stateful, Subscription,
    Task, UniformListScrollHandle, Window, actions, deferred, div, prelude::*, px, relative,
    uniform_list,
};

use crate::context_menu::ContextMenu;
use crate::dialog::Dialog;
use crate::i18n::{tr, trf};
use crate::icons::{FileIcon, ICON_SIZE, IconName, file_icon, folder_icon, icon};
use crate::input::{InputEvent, TextInput};
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, RADIUS_MD, RADIUS_SM};

const ROW_HEIGHT: f32 = 26.;
/// Title bar: the project label and icon buttons.
const HEADER_HEIGHT: f32 = 40.;
/// Rows are inset from the island's edges; the highlight is a rounded box inside the row.
const ROW_INSET: f32 = 6.;
/// Left inset of the box, and the indent per nesting level.
const ROW_PADDING: f32 = 6.;
const INDENT: f32 = 14.;
/// Chevron column: it is empty for files, so icons and names line up with those of directories. The
/// indent guide runs through its middle.
const CHEVRON_WIDTH: f32 = 16.;
const CHEVRON_SIZE: f32 = 12.;
/// Gaps: chevron to icon, icon to name.
const ICON_GAP: f32 = 2.;
const NAME_GAP: f32 = 6.;
const TEXT_SIZE: f32 = 13.;
/// Icon opacity in a muted row (excluded by .gitignore, or cut).
const MUTED_ICON_ALPHA: f32 = 0.45;
/// The width handle occupies the gap between islands to the right of the panel: from the island's
/// border (1 px) for the width of the gap. Its center is that much to the right of the panel's
/// edge.
const RESIZE_HANDLE_WIDTH: f32 = ui::GAP;
const RESIZE_HANDLE_OFFSET: f32 = 1. + RESIZE_HANDLE_WIDTH / 2.;
/// Row hover group: the box is highlighted when the mouse is over the row, including its insets.
const ROW_GROUP: &str = "file-tree-row";
/// Watcher event coalescing: after the first event, wait for the rest to arrive.
const WATCH_DELAY: Duration = Duration::from_millis(80);
/// PageUp/PageDown while the list height is unknown.
const DEFAULT_PAGE_ROWS: usize = 20;

// Window-level actions: handled by Workspace.
actions!(file_tree, [ToggleOpen, ToggleFocus]);

// Panel actions (context "FileTree").
actions!(
    file_tree,
    [
        SelectNext,
        SelectPrevious,
        SelectFirst,
        SelectLast,
        SelectNextPage,
        SelectPreviousPage,
        Expand,
        Collapse,
        Open,
        OpenKeepFocus,
        NewFile,
        NewFolder,
        Rename,
        Duplicate,
        MoveToTrash,
        Cut,
        Copy,
        Paste,
        CopyPath,
        CopyRelativePath,
        RevealInFinder,
        CollapseAll,
        ShowContextMenu,
        Cancel,
        ConfirmEdit,
        CancelEdit,
    ]
);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        // As the Project tool window in JetBrains IDEs; cmd-b goes to a definition.
        KeyBinding::new("cmd-1", ToggleOpen, Some("Workspace")),
        KeyBinding::new("cmd-shift-e", ToggleFocus, Some("Workspace")),
    ]);
    // While a name is being edited, the tree's keys are inactive: arrow keys, Space, ⌫ and ⌘C/⌘V
    // belong to the field.
    let tree = Some("FileTree && !editing");
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, tree),
        KeyBinding::new("up", SelectPrevious, tree),
        KeyBinding::new("home", SelectFirst, tree),
        KeyBinding::new("end", SelectLast, tree),
        KeyBinding::new("pagedown", SelectNextPage, tree),
        KeyBinding::new("pageup", SelectPreviousPage, tree),
        KeyBinding::new("right", Expand, tree),
        KeyBinding::new("left", Collapse, tree),
        KeyBinding::new("enter", Open, tree),
        KeyBinding::new("cmd-down", Open, tree),
        KeyBinding::new("space", OpenKeepFocus, tree),
        KeyBinding::new("cmd-n", NewFile, tree),
        KeyBinding::new("alt-cmd-n", NewFolder, tree),
        KeyBinding::new("f2", Rename, tree),
        // Rename as in JetBrains IDEs.
        KeyBinding::new("shift-f6", Rename, tree),
        KeyBinding::new("cmd-d", Duplicate, tree),
        // The primary binding goes first: the context menu and the palette show it.
        KeyBinding::new("cmd-backspace", MoveToTrash, tree),
        KeyBinding::new("backspace", MoveToTrash, tree),
        KeyBinding::new("delete", MoveToTrash, tree),
        KeyBinding::new("cmd-x", Cut, tree),
        KeyBinding::new("cmd-c", Copy, tree),
        KeyBinding::new("cmd-v", Paste, tree),
        KeyBinding::new("alt-cmd-c", CopyPath, tree),
        KeyBinding::new("alt-shift-cmd-c", CopyRelativePath, tree),
        KeyBinding::new("alt-cmd-r", RevealInFinder, tree),
        KeyBinding::new("shift-f10", ShowContextMenu, tree),
        KeyBinding::new("escape", Cancel, tree),
    ]);
    // The name edit field hands Enter and Esc over to the row.
    let edit = Some("FileTreeEdit");
    cx.bind_keys([
        KeyBinding::new("enter", ConfirmEdit, edit),
        KeyBinding::new("escape", CancelEdit, edit),
    ]);
}

/// What the panel asks Workspace to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileTreeEvent {
    /// Opens a file (or activates its tab); `focus` moves focus to the editor, otherwise it stays
    /// in the tree.
    Open { path: PathBuf, focus: bool },
    /// Returns focus to the editor (Esc in the tree).
    FocusEditor,
    /// The file or directory `from` was renamed or moved to `to`: open documents inside it move to
    /// the new paths.
    Moved { from: PathBuf, to: PathBuf },
    /// Files and directories were deleted to the Trash: tabs of documents inside them are closed
    /// (modified ones stay open).
    Removed { paths: Vec<PathBuf> },
    /// An operation failed (or the project can't be watched): an error notification — `title`
    /// says what, `body` why.
    Error {
        title: SharedString,
        body: SharedString,
    },
    /// Files changed on disk (`None`: events were lost, anything may have): open documents follow.
    DiskChanged(Option<Vec<PathBuf>>),
}

/// In-place name editing: a new file or directory, or a rename.
struct Edit {
    target: EditTarget,
    input: Entity<TextInput>,
    /// Why the operation failed (invalid name, name already exists): shown in red under the field,
    /// and the field stays.
    error: Option<SharedString>,
    /// An operation is already in progress, so a repeated ↵ does nothing.
    pending: bool,
    _subscriptions: [Subscription; 2],
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EditTarget {
    /// A new file or directory in `dir`: the field row goes first among its children.
    New {
        dir: PathBuf,
        kind: EntryKind,
    },
    Rename {
        path: PathBuf,
    },
}

/// What was cut (⌘X) or copied (⌘C), until it is pasted.
#[derive(Debug, Clone)]
struct Clipboard {
    path: PathBuf,
    cut: bool,
}

struct Menu {
    menu: Entity<ContextMenu>,
    /// The click point in window coordinates.
    position: Point<Pixels>,
    _subscriptions: [Subscription; 2],
}

/// The row being dragged; it also serves as the label next to the cursor (icon and name).
#[derive(Debug, Clone)]
pub(crate) struct DraggedEntry {
    pub(crate) path: PathBuf,
    name: SharedString,
    is_dir: bool,
}

/// Dragging the panel's right edge.
#[derive(Debug, Clone, Copy)]
struct DraggedEdge;

/// File tree panel: one per window; it lives as long as the window keeps the same project root.
pub struct FileTreePanel {
    root: PathBuf,
    tree: FileTree,
    /// Visible rows, rebuilt on every tree change.
    rows: Vec<Row>,
    /// The selected row, tracked by path, so it survives directory re-reads.
    selected: Option<PathBuf>,
    /// Select this path as soon as the directories leading to it have been read (reveal, new file).
    reveal_target: Option<PathBuf>,
    focus_handle: FocusHandle,
    scroll: UniformListScrollHandle,
    /// Shared with the commit window, which takes the tree's place in the left island.
    width: ui::LeftIslandWidth,
    /// Reads in flight: the number of the latest request per directory.
    reads: HashMap<PathBuf, u64>,
    next_read: u64,
    edit: Option<Edit>,
    clipboard: Option<Clipboard>,
    menu: Option<Menu>,
    /// Where the dragged row would be dropped: the directory under the cursor.
    drop_target: Option<PathBuf>,
    /// The width is being dragged: the handle stays highlighted while the drag lasts.
    resizing: bool,
    _watcher: Option<Watcher>,
    _watch_task: Task<()>,
    /// Git of the project: names are colored by their change (set by the workspace).
    git: Option<Entity<crate::git::GitStore>>,
    _git_subscription: Option<Subscription>,
    /// The window's plugins: the context menu shows their items (part 8.2).
    plugins: Option<Entity<crate::plugins::PluginStore>>,
}

impl EventEmitter<FileTreeEvent> for FileTreePanel {}

impl FileTreePanel {
    /// Panel for the project at `root` (a canonical path): reads the root right away and starts
    /// watching for changes on disk.
    pub fn new(
        root: PathBuf,
        width: ui::LeftIslandWidth,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let watch_task = Self::watch(root.clone(), cx);
        let mut panel = Self {
            tree: FileTree::new(root.clone()),
            root,
            rows: Vec::new(),
            selected: None,
            reveal_target: None,
            focus_handle: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            width,
            reads: HashMap::new(),
            next_read: 0,
            edit: None,
            clipboard: None,
            menu: None,
            drop_target: None,
            resizing: false,
            _watcher: None,
            _watch_task: watch_task,
            git: None,
            _git_subscription: None,
            plugins: None,
        };
        panel.changed(cx);
        panel
    }

    /// Git of the project: file and directory names follow their changes.
    pub fn set_git(&mut self, git: Entity<crate::git::GitStore>, cx: &mut Context<Self>) {
        self._git_subscription = Some(cx.observe(&git, |_, _, cx| cx.notify()));
        self.git = Some(git);
        cx.notify();
    }

    /// The window's plugins: their items in the context menu.
    pub fn set_plugins(&mut self, plugins: Entity<crate::plugins::PluginStore>) {
        self.plugins = Some(plugins);
    }

    /// The active tab's file: expands the directories leading to it, selects it and scrolls to it.
    /// For a file outside the project root, the selection is cleared.
    pub fn reveal(&mut self, path: &Path, cx: &mut Context<Self>) {
        if path == self.root || !self.tree.reveal(path) {
            self.selected = None;
            self.reveal_target = None;
            return cx.notify();
        }
        self.reveal_target = Some(path.to_path_buf());
        self.changed(cx);
    }

    // --- Reading directories ---

    /// The tree changed: rebuild the rows, finish reading what is visible, advance the reveal,
    /// repaint.
    fn changed(&mut self, cx: &mut Context<Self>) {
        self.rows = self.tree.rows();
        for (dir, ignored) in self.tree.pending_loads() {
            if !self.reads.contains_key(&dir) {
                self.read(dir, ignored, cx);
            }
        }
        self.finish_reveal();
        cx.notify();
    }

    /// Re-reads directories after the panel's own operations and after changes on disk.
    fn reread(&mut self, dirs: impl IntoIterator<Item = PathBuf>, cx: &mut Context<Self>) {
        for dir in dirs {
            let ignored = self.tree.is_ignored(&dir);
            self.read(dir, ignored, cx);
        }
    }

    /// Reads a directory in the background. A new request for the same directory makes the previous
    /// one stale.
    fn read(&mut self, dir: PathBuf, ignored: bool, cx: &mut Context<Self>) {
        self.next_read += 1;
        let id = self.next_read;
        self.reads.insert(dir.clone(), id);
        let listing = cx.background_spawn({
            let (root, dir) = (self.root.clone(), dir.clone());
            async move { list_dir(&root, &dir, ignored) }
        });
        cx.spawn(async move |this, cx| {
            let result = listing.await;
            this.update(cx, |this, cx| this.finish_read(dir, id, result, cx))
                .ok();
        })
        .detach();
    }

    fn finish_read(
        &mut self,
        dir: PathBuf,
        id: u64,
        result: io::Result<Vec<DirEntry>>,
        cx: &mut Context<Self>,
    ) {
        if self.reads.get(&dir) != Some(&id) {
            return;
        }
        self.reads.remove(&dir);
        let entries = match result {
            Ok(entries) => entries,
            // The directory was deleted: its parent is re-read on the same event.
            Err(err) if err.kind() == io::ErrorKind::NotFound && dir != self.root => {
                self.tree.remove(&dir);
                return self.changed(cx);
            }
            // If the re-read fails, the previous listing stays.
            Err(_) if self.tree.is_loaded(&dir) => return self.changed(cx),
            Err(err) => {
                cx.emit(FileTreeEvent::Error {
                    title: trf("Cannot read {0}", &[&display_name(&dir)]).into(),
                    body: err.to_string().into(),
                });
                Vec::new()
            }
        };
        let stale = self.tree.set_listing(&dir, entries);
        self.reread(stale, cx);
        self.changed(cx);
    }

    /// Selects `reveal_target` once its row has appeared; if everything has been read and there is
    /// still no row, the target is cleared.
    fn finish_reveal(&mut self) {
        let Some(target) = &self.reveal_target else {
            return;
        };
        if let Some(index) = self.row_index(target) {
            self.selected = Some(target.clone());
            let index = self.list_index(index);
            self.scroll.scroll_to_item(index, ScrollStrategy::Center);
            self.reveal_target = None;
        } else if self.reads.is_empty() {
            self.reveal_target = None;
        }
    }

    /// Selects `path` as soon as the directories leading to it have been read.
    fn select_when_shown(&mut self, path: PathBuf) {
        self.tree.reveal(&path);
        self.reveal_target = Some(path);
    }

    /// Watching the root: events are coalesced into a batch and turned into a re-read plan.
    fn watch(root: PathBuf, cx: &mut Context<Self>) -> Task<()> {
        let (sender, mut changes) = mpsc::unbounded::<FsChange>();
        let started = cx.background_spawn(async move {
            Watcher::new(&root, move |change| {
                sender.unbounded_send(change).ok();
            })
        });
        cx.spawn(async move |this, cx| {
            let watcher = match started.await {
                Ok(watcher) => watcher,
                Err(err) => {
                    let event = FileTreeEvent::Error {
                        title: tr("Not watching the project for changes").into(),
                        body: err.to_string().into(),
                    };
                    this.update(cx, |_, cx| cx.emit(event)).ok();
                    return;
                }
            };
            if this
                .update(cx, |this, _| this._watcher = Some(watcher))
                .is_err()
            {
                return;
            }
            while let Some(first) = changes.next().await {
                cx.background_executor().timer(WATCH_DELAY).await;
                let mut batch = vec![first];
                while let Ok(change) = changes.try_recv() {
                    batch.push(change);
                }
                let refreshed = this.update(cx, |this, cx| {
                    let plan = this.tree.refresh_plan(&batch);
                    this.reread(plan, cx);
                    let mut paths = Vec::new();
                    let mut rescan = false;
                    for change in &batch {
                        match change {
                            FsChange::Paths(changed) => paths.extend(changed.iter().cloned()),
                            FsChange::Rescan => rescan = true,
                        }
                    }
                    cx.emit(FileTreeEvent::DiskChanged((!rescan).then_some(paths)));
                });
                if refreshed.is_err() {
                    break;
                }
            }
        })
    }

    // --- Selection and navigation ---

    fn row_index(&self, path: &Path) -> Option<usize> {
        self.rows.iter().position(|row| row.path == path)
    }

    fn selected_index(&self) -> Option<usize> {
        self.selected
            .as_deref()
            .and_then(|path| self.row_index(path))
    }

    /// The new-file field row: its position in the list, nesting depth and kind.
    fn new_entry_slot(&self) -> Option<(usize, usize, EntryKind)> {
        let Some(Edit {
            target: EditTarget::New { dir, kind },
            ..
        }) = &self.edit
        else {
            return None;
        };
        if *dir == self.root {
            return Some((0, 0, *kind));
        }
        let index = self.row_index(dir)?;
        Some((index + 1, self.rows[index].depth + 1, *kind))
    }

    /// List index for a tree row: the new-file field shifts the rows after it.
    fn list_index(&self, row: usize) -> usize {
        match self.new_entry_slot() {
            Some((at, ..)) if row >= at => row + 1,
            _ => row,
        }
    }

    fn select_row(&mut self, index: usize, strategy: ScrollStrategy, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index) else {
            return;
        };
        self.selected = Some(row.path.clone());
        self.reveal_target = None;
        self.scroll.scroll_to_item(self.list_index(index), strategy);
        cx.notify();
    }

    /// Rows per page, based on the list height in the previous frame.
    fn page_rows(&self) -> usize {
        let height = f32::from(self.scroll.0.borrow().base_handle.bounds().size.height);
        if height <= 0. {
            return DEFAULT_PAGE_ROWS;
        }
        ((height / ROW_HEIGHT) as usize).saturating_sub(1).max(1)
    }

    /// Scrolls by the minimum amount: down, the row ends up at the bottom edge; up, at the top
    /// edge.
    fn move_selection(
        &mut self,
        to: impl FnOnce(Option<usize>, usize) -> usize,
        cx: &mut Context<Self>,
    ) {
        if self.rows.is_empty() {
            return;
        }
        let current = self.selected_index();
        let index = to(current, self.rows.len()).min(self.rows.len() - 1);
        let strategy = match current {
            Some(current) if index < current => ScrollStrategy::Top,
            _ => ScrollStrategy::Bottom,
        };
        self.select_row(index, strategy, cx);
    }

    /// → : expands a collapsed directory; on an expanded one, moves to the first child.
    fn expand(&mut self, _: &Expand, _: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected_index() else {
            return;
        };
        let row = &self.rows[index];
        if row.kind != EntryKind::Dir {
            return;
        }
        if !row.expanded {
            let path = row.path.clone();
            self.tree.expand(&path);
            return self.changed(cx);
        }
        if self
            .rows
            .get(index + 1)
            .is_some_and(|next| next.depth > row.depth)
        {
            self.select_row(index + 1, ScrollStrategy::Bottom, cx);
        }
    }

    /// ← : collapses an expanded directory; otherwise moves to the parent.
    fn collapse(&mut self, _: &Collapse, _: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected_index() else {
            return;
        };
        let row = &self.rows[index];
        if row.kind == EntryKind::Dir && row.expanded {
            let path = row.path.clone();
            self.tree.collapse(&path);
            return self.changed(cx);
        }
        let parent = row.path.parent().and_then(|parent| self.row_index(parent));
        if let Some(parent) = parent {
            self.select_row(parent, ScrollStrategy::Top, cx);
        }
    }

    /// ↵, Space, click: expands or collapses a directory; opens a file.
    fn activate(&mut self, path: PathBuf, focus: bool, cx: &mut Context<Self>) {
        if self.tree.is_dir(&path) {
            self.tree.toggle(&path);
            self.keep_selection_visible();
            self.changed(cx);
        } else {
            cx.emit(FileTreeEvent::Open { path, focus });
        }
    }

    /// The selected row got hidden inside a collapsed directory: select the nearest visible
    /// directory above it.
    fn keep_selection_visible(&mut self) {
        let Some(selected) = &self.selected else {
            return;
        };
        let rows = self.tree.rows();
        if rows.iter().any(|row| &row.path == selected) {
            return;
        }
        self.selected = selected
            .ancestors()
            .skip(1)
            .find(|dir| rows.iter().any(|row| row.path == *dir))
            .map(Path::to_path_buf);
    }

    fn collapse_all(&mut self, _: &CollapseAll, _: &mut Window, cx: &mut Context<Self>) {
        self.tree.collapse_all();
        self.keep_selection_visible();
        self.changed(cx);
    }

    /// Esc: clears the "cut" state; otherwise moves focus to the editor.
    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        if self
            .clipboard
            .as_ref()
            .is_some_and(|clipboard| clipboard.cut)
        {
            self.clipboard = None;
            return cx.notify();
        }
        cx.emit(FileTreeEvent::FocusEditor);
    }

    // --- Name editing: new file, new directory, rename ---

    fn new_entry(&mut self, kind: EntryKind, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.tree.target_dir(self.selected.as_deref());
        if dir != self.root {
            self.tree.reveal(&dir);
            self.tree.expand(&dir);
        }
        self.start_edit(EditTarget::New { dir, kind }, "", None, window, cx);
    }

    fn start_rename(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let name = display_name(&path);
        let stem = stem_range(&name, self.tree.is_dir(&path));
        self.start_edit(EditTarget::Rename { path }, &name, Some(stem), window, cx);
    }

    fn start_edit(
        &mut self,
        target: EditTarget,
        text: &str,
        select: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let placeholder = match &target {
            EditTarget::New {
                kind: EntryKind::Dir,
                ..
            } => tr("Folder name"),
            EditTarget::New { .. } => tr("File name"),
            EditTarget::Rename { .. } => "",
        };
        let input = cx.new(|cx| {
            let mut input = TextInput::new(placeholder, cx).compact();
            input.set_text(text, cx);
            match select {
                Some(range) => input.select_range(range, cx),
                None => input.select_all(cx),
            }
            input
        });
        let focus = input.focus_handle(cx);
        // Nothing on disk changes without ↵: losing focus cancels.
        let subscriptions = [
            cx.on_focus_out(&focus, window, |this, _, window, cx| {
                this.cancel_edit(false, window, cx)
            }),
            cx.subscribe(&input, |this, _, _: &InputEvent, cx| {
                if let Some(edit) = &mut this.edit {
                    edit.error = None;
                }
                cx.notify();
            }),
        ];
        window.focus(&focus);
        self.edit = Some(Edit {
            target,
            input,
            error: None,
            pending: false,
            _subscriptions: subscriptions,
        });
        self.changed(cx);
        let index = match self.new_entry_slot() {
            Some((at, ..)) => Some(at),
            None => self.selected_index().map(|row| self.list_index(row)),
        };
        if let Some(index) = index {
            self.scroll.scroll_to_item(index, ScrollStrategy::Center);
        }
    }

    /// Closes the field without changing anything on disk. `refocus` returns focus to the tree
    /// (Esc); when focus is lost, it is already wherever it was moved to.
    fn cancel_edit(&mut self, refocus: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.edit.take().is_none() {
            return;
        }
        if refocus {
            window.focus(&self.focus_handle);
        }
        self.changed(cx);
    }

    fn confirm_edit(&mut self, _: &ConfirmEdit, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = &mut self.edit else {
            return;
        };
        if edit.pending {
            return;
        }
        let name = edit.input.read(cx).text().trim().to_string();
        let target = edit.target.clone();
        if let Err(error) = check_name(&target, &name) {
            edit.error = Some(error.into());
            return cx.notify();
        }
        if let EditTarget::Rename { path } = &target
            && display_name(path) == name
        {
            return self.cancel_edit(true, window, cx);
        }
        edit.pending = true;
        let op = target.clone();
        self.run(
            move || match op {
                EditTarget::New {
                    dir,
                    kind: EntryKind::File,
                } => create_file(&dir, &name),
                EditTarget::New {
                    dir,
                    kind: EntryKind::Dir,
                } => create_dir(&dir, &name),
                EditTarget::Rename { path } => rename(&path, &name),
            },
            window,
            cx,
            |this, result, window, cx| this.finish_edit(target, result, window, cx),
        );
    }

    fn finish_edit(
        &mut self,
        target: EditTarget,
        result: io::Result<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editing = self.edit.as_ref().is_some_and(|edit| edit.target == target);
        let path = match result {
            Ok(path) => path,
            Err(err) => {
                match &mut self.edit {
                    Some(edit) if editing => {
                        edit.pending = false;
                        edit.error = Some(err.to_string().into());
                    }
                    _ => cx.emit(FileTreeEvent::Error {
                        title: match target {
                            EditTarget::Rename { .. } => tr("Couldn't rename").into(),
                            _ => tr("Couldn't create").into(),
                        },
                        body: err.to_string().into(),
                    }),
                }
                return cx.notify();
            }
        };
        if editing {
            self.edit = None;
            window.focus(&self.focus_handle);
        }
        match target {
            EditTarget::New { dir, kind } => {
                self.reread([dir], cx);
                self.select_when_shown(path.clone());
                if kind == EntryKind::File {
                    cx.emit(FileTreeEvent::Open { path, focus: true });
                }
            }
            EditTarget::Rename { path: from } => self.moved(&from, &path, cx),
        }
        self.changed(cx);
    }

    /// `from` has moved to `to` (rename, move): the tree, the selection and the open documents
    /// follow; both directories are re-read. The same path (same name, same directory) means the
    /// operation did nothing, so only the selection changes.
    fn moved(&mut self, from: &Path, to: &Path, cx: &mut Context<Self>) {
        if from == to {
            return self.select_when_shown(to.to_path_buf());
        }
        self.tree.rename(from, to);
        if let Some(clipboard) = &mut self.clipboard
            && let Some(path) = remap(&clipboard.path, from, to)
        {
            clipboard.path = path;
        }
        let dirs: BTreeSet<PathBuf> = [from.parent(), to.parent()]
            .into_iter()
            .flatten()
            .map(Path::to_path_buf)
            .collect();
        self.reread(dirs, cx);
        self.select_when_shown(to.to_path_buf());
        cx.emit(FileTreeEvent::Moved {
            from: from.to_path_buf(),
            to: to.to_path_buf(),
        });
    }

    // --- Operations ---

    /// Runs a file operation in the background; `done` is called with the result on the UI thread.
    fn run<T: Send + 'static>(
        &mut self,
        op: impl FnOnce() -> io::Result<T> + Send + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, io::Result<T>, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let task = cx.background_spawn(async move { op() });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| done(this, result, window, cx))
                .ok();
        })
        .detach();
    }

    fn move_to_trash(&mut self, _: &MoveToTrash, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.selected.clone() else {
            return;
        };
        let name = display_name(&path);
        let detail = if self.tree.is_dir(&path) {
            tr("The folder and everything in it will be moved to the Trash.")
        } else {
            tr("You can restore it from the Trash.")
        };
        let answer = Dialog::warning(trf("Move “{0}” to Trash?", &[&name]))
            .message(detail)
            .danger(tr("Move to Trash"))
            .cancel(tr("Cancel"))
            .show(window, cx);
        cx.spawn_in(window, async move |this, cx| {
            if answer.await != Some(0) {
                return;
            }
            this.update_in(cx, |this, window, cx| {
                let trashed = path.clone();
                this.run(
                    move || trash(std::slice::from_ref(&trashed)),
                    window,
                    cx,
                    move |this, result, _, cx| this.finish_trash(path, result, cx),
                );
            })
            .ok();
        })
        .detach();
    }

    fn finish_trash(&mut self, path: PathBuf, result: io::Result<()>, cx: &mut Context<Self>) {
        if let Err(err) = result {
            return cx.emit(FileTreeEvent::Error {
                title: tr("Couldn't move to Trash").into(),
                body: err.to_string().into(),
            });
        }
        // The selection moves to a neighbor: the next row, or the previous one for the last row.
        let index = self.row_index(&path);
        self.tree.remove(&path);
        if self
            .selected
            .as_ref()
            .is_some_and(|selected| selected.starts_with(&path))
        {
            let rows = self.tree.rows();
            self.selected = index
                .and_then(|index| rows.get(index.min(rows.len().saturating_sub(1))))
                .map(|row| row.path.clone());
        }
        if self
            .clipboard
            .as_ref()
            .is_some_and(|c| c.path.starts_with(&path))
        {
            self.clipboard = None;
        }
        self.reread(path.parent().map(Path::to_path_buf), cx);
        cx.emit(FileTreeEvent::Removed { paths: vec![path] });
        self.changed(cx);
    }

    fn set_clipboard(&mut self, cut: bool, cx: &mut Context<Self>) {
        if let Some(path) = self.selected.clone() {
            self.clipboard = Some(Clipboard { path, cut });
            cx.notify();
        }
    }

    /// ⌘V: what was cut moves into the selected directory (for a file, into its directory); what
    /// was copied is copied there under an unused name.
    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        let Some(clipboard) = self.clipboard.clone() else {
            return;
        };
        let dir = self.tree.target_dir(self.selected.as_deref());
        if clipboard.cut {
            self.clipboard = None;
            self.move_entry(clipboard.path, dir, window, cx);
        } else {
            self.copy_entry(clipboard.path, dir, false, window, cx);
        }
    }

    fn move_entry(
        &mut self,
        path: PathBuf,
        dir: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if path.parent() == Some(dir.as_path()) {
            return cx.notify();
        }
        let from = path.clone();
        self.run(
            move || move_into(&path, &dir),
            window,
            cx,
            move |this, result, _, cx| match result {
                Ok(to) => {
                    this.moved(&from, &to, cx);
                    this.changed(cx);
                }
                Err(err) => cx.emit(FileTreeEvent::Error {
                    title: tr("Couldn't move").into(),
                    body: err.to_string().into(),
                }),
            },
        );
    }

    /// Copies into `dir`; `rename_after` starts renaming the copy right away (⌘D).
    fn copy_entry(
        &mut self,
        path: PathBuf,
        dir: PathBuf,
        rename_after: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = dir.clone();
        self.run(
            move || copy_into(&path, &dir),
            window,
            cx,
            move |this, result, window, cx| match result {
                Ok(copy) => {
                    this.reread([target], cx);
                    this.select_when_shown(copy.clone());
                    this.changed(cx);
                    if rename_after {
                        this.rename_when_shown(copy, window, cx);
                    }
                }
                Err(err) => cx.emit(FileTreeEvent::Error {
                    title: tr("Couldn't copy").into(),
                    body: err.to_string().into(),
                }),
            },
        );
    }

    /// Renaming the copy: its row will appear after the directory is re-read.
    fn rename_when_shown(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.row_index(&path).is_some() {
            return self.start_rename(path, window, cx);
        }
        cx.spawn_in(window, async move |this, cx| {
            // Re-reading takes a fraction of a millisecond; we wait a few frames at most.
            for _ in 0..30 {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                let shown = this
                    .update_in(cx, |this, window, cx| {
                        let shown = this.row_index(&path).is_some();
                        if shown && this.edit.is_none() {
                            this.start_rename(path.clone(), window, cx);
                        }
                        shown
                    })
                    .unwrap_or(true);
                if shown {
                    break;
                }
            }
        })
        .detach();
    }

    fn duplicate(&mut self, _: &Duplicate, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.selected.clone() else {
            return;
        };
        let dir = self.tree.target_dir(path.parent());
        self.copy_entry(path, dir, true, window, cx);
    }

    /// ⌥⌘C and ⌥⇧⌘C: the path of the selected item (of the root, if nothing is selected) to the
    /// clipboard.
    fn copy_path(&mut self, relative: bool, cx: &mut Context<Self>) {
        let path = self.selected.clone().unwrap_or_else(|| self.root.clone());
        let text = if relative {
            relative_path(&self.root, &path)
        } else {
            path.display().to_string()
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    fn reveal_in_finder(&mut self, _: &RevealInFinder, _: &mut Window, cx: &mut Context<Self>) {
        let path = self.selected.clone().unwrap_or_else(|| self.root.clone());
        cx.reveal_path(&path);
    }

    // --- Mouse ---

    /// Clicking a row works like in VS Code: the file opens, but focus stays in the tree (so you
    /// can keep browsing); double-clicking a file moves focus to the editor. A directory expands or
    /// collapses on the first click; the second click of a double-click leaves it alone.
    fn click_row(
        &mut self,
        path: PathBuf,
        click_count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        self.selected = Some(path.clone());
        self.reveal_target = None;
        if !self.tree.is_dir(&path) {
            cx.emit(FileTreeEvent::Open {
                path,
                focus: click_count >= 2,
            });
        } else if click_count == 1 {
            self.activate(path, false, cx);
        }
        cx.notify();
    }

    /// Right-click: selects the row (`None` means empty space, i.e. the root) and opens the menu at
    /// the cursor.
    fn secondary_click(
        &mut self,
        path: Option<PathBuf>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        self.selected = path.clone();
        self.reveal_target = None;
        let can_paste = self.clipboard.is_some();
        // The plugins' items: for the row, or for the root on empty space (part 8.2).
        let plugins = self.plugins.clone();
        let target = crate::plugin_menus::MenuTarget {
            location: flux_plugin::manifest::MenuLocation::Tree,
            selection: false,
            paths: vec![path.clone().unwrap_or_else(|| self.root.clone())],
        };
        // Git: the history of a file or a directory of a repository (not of an untracked one).
        let history = path.clone().filter(|path| {
            self.git.as_ref().is_some_and(|git| {
                let git = git.read(cx);
                git.repo_index(path).is_some()
                    && git.status_of(path) != Some(flux_git::FileStatus::Untracked)
            })
        });
        let menu = cx.new(|cx| {
            let menu = ContextMenu::new(window, cx)
                .entry(tr("New File"), NewFile)
                .entry(tr("New Folder"), NewFolder)
                .separator();
            let menu = match path {
                Some(_) => menu
                    .entry(tr("Rename"), Rename)
                    .entry(tr("Duplicate"), Duplicate)
                    .entry(tr("Move to Trash"), MoveToTrash)
                    .separator()
                    .entry(tr("Cut"), Cut)
                    .entry(tr("Copy"), Copy)
                    .entry_if(can_paste, tr("Paste"), Paste)
                    .separator()
                    .entry(tr("Copy Path"), CopyPath)
                    .entry(tr("Copy Relative Path"), CopyRelativePath),
                None => menu
                    .entry_if(can_paste, tr("Paste"), Paste)
                    .entry(tr("Collapse All"), CollapseAll)
                    .separator()
                    .entry(tr("Copy Path"), CopyPath),
            };
            let menu = match history {
                Some(path) => menu
                    .separator()
                    .entry(
                        tr("Show History"),
                        crate::git::ShowHistory { path, lines: None },
                    )
                    .separator(),
                None => menu,
            };
            let menu = menu.entry(tr("Reveal in Finder"), RevealInFinder);
            // Claude: the file or the folder as a mention in the current chat's message.
            let menu = match path.filter(|_| crate::claude_actions::offered(cx)) {
                Some(path) => menu.separator().entry(
                    tr("Send to Claude"),
                    crate::claude_actions::SendPathsToClaude(vec![path]),
                ),
                None => menu,
            };
            match &plugins {
                Some(plugins) => crate::plugin_menus::append(menu, plugins, &target, cx),
                None => menu,
            }
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
        self.menu = Some(Menu {
            menu,
            position,
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    /// Closes the menu (if it is still the same one); Esc in the menu returns focus to the tree.
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

    /// Focus is in the tree when it is on the tree itself, on the name edit field or on its menu.
    /// The field and the menu are checked directly: in the first frame after creation they are not
    /// in the focus tree yet.
    fn is_focused(&self, window: &Window, cx: &App) -> bool {
        self.focus_handle.contains_focused(window, cx)
            || self
                .edit
                .as_ref()
                .is_some_and(|edit| edit.input.focus_handle(cx).is_focused(window))
            || self
                .menu
                .as_ref()
                .is_some_and(|menu| menu.menu.focus_handle(cx).is_focused(window))
    }

    /// ⇧F10: the menu for the selected row (for the root, if nothing is selected), opened below it,
    /// as after a click.
    fn show_context_menu(
        &mut self,
        _: &ShowContextMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (bounds, offset) = {
            let state = self.scroll.0.borrow();
            (state.base_handle.bounds(), state.base_handle.offset())
        };
        let row = self
            .selected_index()
            .map(|index| (self.list_index(index), &self.rows[index]));
        let (top, depth) = match row {
            Some((index, row)) => (index as f32 + 1., row.depth),
            None => (0., 0),
        };
        let position = gpui::point(
            bounds.left() + px(name_offset(depth)),
            bounds.top() + offset.y + px(top * ROW_HEIGHT),
        );
        self.secondary_click(self.selected.clone(), position, window, cx);
    }

    fn set_drop_target(&mut self, target: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.drop_target != target {
            self.drop_target = target;
            cx.notify();
        }
    }

    /// The list row under a window point, based on the previous frame's layout.
    fn list_index_at(&self, position: Point<Pixels>) -> Option<usize> {
        let state = self.scroll.0.borrow();
        let bounds = state.base_handle.bounds();
        if !bounds.contains(&position) {
            return None;
        }
        let y = f32::from(position.y - bounds.top() - state.base_handle.offset().y);
        let index = (y / ROW_HEIGHT).floor();
        (index >= 0.).then_some(index as usize)
    }

    /// Dragging over the panel: the target is the directory of the row under the cursor (for a
    /// file, its directory); below the rows and over the title bar, the root; outside the panel,
    /// nowhere.
    fn drag_over(&mut self, event: &DragMoveEvent<DraggedEntry>, cx: &mut Context<Self>) {
        let dragged = event.drag(cx).path.clone();
        let position = event.event.position;
        let target = event.bounds.contains(&position).then(|| {
            let over = self
                .list_index_at(position)
                .and_then(|index| self.list_row(index))
                .map(|row| row.path.clone());
            drop_dir(&self.tree, &dragged, over.as_deref())
        });
        self.set_drop_target(target.flatten(), cx);
    }

    fn drop_entry(&mut self, entry: &DraggedEntry, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dir) = self.drop_target.take() else {
            return;
        };
        cx.notify();
        self.move_entry(entry.path.clone(), dir, window, cx);
    }

    /// The tree row at a list index; for the new-file field row, `None`.
    fn list_row(&self, index: usize) -> Option<&Row> {
        match self.new_entry_slot() {
            Some((at, ..)) if index == at => None,
            Some((at, ..)) if index > at => self.rows.get(index - 1),
            _ => self.rows.get(index),
        }
    }

    // --- Rendering ---

    /// Title bar: the "new file", "new folder" and "collapse all" buttons on the right (the project's
    /// name is in the window's title bar, not repeated here). A drop on it moves into the root, a
    /// right click opens the root's menu.
    fn render_header(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let dropping_root = self.drop_target.as_ref() == Some(&self.root);
        div()
            .id("file-tree-header")
            .relative()
            .flex_none()
            .h(px(HEADER_HEIGHT))
            .pl(px(ROW_INSET + ROW_PADDING + 2.))
            .pr(px(ROW_INSET))
            .flex()
            .items_center()
            .gap_0p5()
            .when(dropping_root, |header| {
                header.child(drop_overlay(ui).top(px(ROW_INSET)).bottom(px(2.)))
            })
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.secondary_click(None, event.position, window, cx)
                }),
            )
            .child(div().flex_1())
            .child(self.header_button(
                ("new-file", IconName::FilePlus, tr("New File")),
                &NewFile,
                window,
                cx,
                |this, window, cx| this.new_entry(EntryKind::File, window, cx),
            ))
            .child(self.header_button(
                ("new-folder", IconName::FolderPlus, tr("New Folder")),
                &NewFolder,
                window,
                cx,
                |this, window, cx| this.new_entry(EntryKind::Dir, window, cx),
            ))
            .child(self.header_button(
                ("collapse-all", IconName::CollapseAll, tr("Collapse All")),
                &CollapseAll,
                window,
                cx,
                |this, window, cx| this.collapse_all(&CollapseAll, window, cx),
            ))
    }

    /// Title bar button: the tooltip shows the key binding from the tree's keymap; a click moves
    /// focus to the tree and runs the action.
    fn header_button(
        &self,
        (id, name, label): (&'static str, IconName, &'static str),
        action: &dyn Action,
        window: &Window,
        cx: &mut Context<Self>,
        run: fn(&mut Self, &mut Window, &mut Context<Self>),
    ) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let keys = ui::shortcut_in(action, &self.focus_handle, window);
        ui::icon_button(id, name, ui)
            .tooltip(ui::tooltip(label, keys))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                window.focus(&this.focus_handle);
                run(this, window, cx)
            }))
    }

    fn render_item(
        &mut self,
        index: usize,
        slot: Option<(usize, usize, EntryKind)>,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match slot {
            Some((at, depth, kind)) if index == at => self.render_new_entry(index, depth, kind, cx),
            Some((at, ..)) if index > at => self.render_row(index - 1, index, focused, cx),
            _ => self.render_row(index, index, focused, cx),
        }
    }

    fn render_row(
        &mut self,
        row_index: usize,
        list_index: usize,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        let Some(row) = self.rows.get(row_index) else {
            return div().into_any_element();
        };
        let selected = self.selected.as_ref() == Some(&row.path);
        let band = self
            .drop_target
            .as_deref()
            .filter(|target| *target != self.root)
            .and_then(|target| drop_band(&self.rows, target, row_index));
        let cut = self
            .clipboard
            .as_ref()
            .is_some_and(|clipboard| clipboard.cut && row.path.starts_with(&clipboard.path));
        let renaming = matches!(
            &self.edit,
            Some(Edit { target: EditTarget::Rename { path }, .. }) if *path == row.path
        );
        let muted = row.ignored || cut;
        // While the name is being edited, the icon follows what has been typed: `.rs` → the Rust
        // icon.
        let typed = self
            .edit
            .as_ref()
            .filter(|_| renaming)
            .map(|edit| edit.input.read(cx).text());
        let mut file = match row.kind {
            EntryKind::Dir => folder_icon(row.expanded, &ui),
            EntryKind::File => file_icon(typed.as_deref().unwrap_or(&row.name), &ui),
        };
        if muted {
            file.color = UiColors::tint(file.color, MUTED_ICON_ALPHA);
        }
        let body = row_body(row.depth, row.kind, row.expanded, file, ui);
        let body = match band {
            Some((top, bottom)) => body
                .bg(ui.drop_target)
                .rounded(px(0.))
                .when(top, |body| body.rounded_t(px(RADIUS_SM)))
                .when(bottom, |body| body.rounded_b(px(RADIUS_SM))),
            None if selected => body.bg(if focused {
                ui.list_selected
            } else {
                ui.list_selected_inactive
            }),
            None => body.group_hover(ROW_GROUP, move |style| style.bg(ui.hover)),
        };
        if renaming {
            return row_shell(list_index)
                .child(body.child(self.render_edit_field(ui)))
                .into_any_element();
        }
        // Git: a changed file in its status color, a directory with changes inside in the modified
        // one, a wholly untracked directory in the untracked one (as the Project view of JetBrains
        // IDEs colors them).
        let status = self.git.as_ref().and_then(|git| {
            let git = git.read(cx);
            match row.kind {
                EntryKind::File => git.status_of(&row.path),
                EntryKind::Dir => git.dir_status(&row.path),
            }
        });
        let color = match status {
            _ if muted => ui.dim,
            Some(status) => crate::git::status_color(status, &ui),
            None => ui.foreground,
        };
        let name = div()
            .ml(px(NAME_GAP))
            .min_w_0()
            .truncate()
            .text_color(color)
            .child(row.name.clone());
        let (click, secondary) = (row.path.clone(), row.path.clone());
        let dragged = DraggedEntry {
            path: row.path.clone(),
            name: row.name.clone().into(),
            is_dir: row.kind == EntryKind::Dir,
        };
        row_shell(list_index)
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.click_row(click.clone(), event.click_count(), window, cx)
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.secondary_click(Some(secondary.clone()), event.position, window, cx)
                }),
            )
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .child(body.child(name))
            .into_any_element()
    }

    /// The row of the new file or directory field: the file icon follows the name being typed.
    fn render_new_entry(
        &self,
        list_index: usize,
        depth: usize,
        kind: EntryKind,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        let typed = self
            .edit
            .as_ref()
            .map(|edit| edit.input.read(cx).text())
            .unwrap_or_default();
        let file = match kind {
            EntryKind::Dir => folder_icon(false, &ui),
            EntryKind::File => file_icon(&typed, &ui),
        };
        row_shell(list_index)
            .child(
                row_body(depth, kind, false, file, ui)
                    .bg(ui.list_selected_inactive)
                    .child(self.render_edit_field(ui)),
            )
            .into_any_element()
    }

    /// Name edit field; an error is shown as a box under the field, on top of the neighboring rows.
    fn render_edit_field(&self, ui: UiColors) -> AnyElement {
        let Some(edit) = &self.edit else {
            return div().into_any_element();
        };
        div()
            .key_context("FileTreeEdit")
            .relative()
            .flex_1()
            .min_w_0()
            .ml(px(NAME_GAP - 2.))
            .child(edit.input.clone())
            .children(edit.error.clone().map(|error| {
                deferred(
                    div()
                        .absolute()
                        .top(relative(1.))
                        .mt_1()
                        .left_0()
                        .min_w(px(180.))
                        .max_w(px(320.))
                        .flex()
                        .items_start()
                        .gap_1p5()
                        .px_2()
                        .py_1p5()
                        .rounded(px(RADIUS_MD))
                        // Opaque: the text of the neighboring rows lies under the box.
                        .bg(UiColors::tint(ui.elevated, 1.))
                        .border_1()
                        .border_color(UiColors::tint(ui.error, 0.6))
                        .shadow(ui::popover_shadow(ui))
                        .whitespace_normal()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.foreground)
                        .child(icon(IconName::Error, ui.error).size(px(13.)).mt(px(1.)))
                        .child(div().min_w_0().child(error)),
                )
                .with_priority(1)
            }))
            .into_any_element()
    }
}

impl Focusable for FileTreePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for FileTreePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let slot = self.new_entry_slot();
        let count = self.rows.len() + usize::from(slot.is_some());
        let list = uniform_list(
            "file-tree-rows",
            count,
            cx.processor(move |this, range: Range<usize>, window, cx| {
                let focused = this.is_focused(window, cx);
                range
                    .map(|index| this.render_item(index, slot, focused, cx))
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(self.scroll.clone())
        .size_full();
        let dropping_root = self.drop_target.as_ref() == Some(&self.root);
        // The handle was released anywhere: the drag is over and the highlight goes away.
        self.resizing &= cx.has_active_drag();
        // A separate context applies while a name is being edited: the tree's keys (arrow keys, ⌫,
        // Space) are inactive.
        let context = if self.edit.is_some() {
            "FileTree editing"
        } else {
            "FileTree"
        };
        div()
            .key_context(context)
            .track_focus(&self.focus_handle)
            .relative()
            .flex_none()
            .w(px(self.width.get()))
            .h_full()
            .flex()
            .flex_col()
            .font_family(theme::UI_FONT)
            .text_size(px(TEXT_SIZE))
            .text_color(ui.foreground)
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| {
                this.move_selection(|current, _| current.map_or(0, |i| i + 1), cx)
            }))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| {
                this.move_selection(|current, _| current.map_or(0, |i| i.saturating_sub(1)), cx)
            }))
            .on_action(
                cx.listener(|this, _: &SelectFirst, _, cx| this.move_selection(|_, _| 0, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectLast, _, cx| {
                    this.move_selection(|_, len| len - 1, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &SelectNextPage, _, cx| {
                let page = this.page_rows();
                this.move_selection(|current, _| current.map_or(0, |i| i + page), cx)
            }))
            .on_action(cx.listener(|this, _: &SelectPreviousPage, _, cx| {
                let page = this.page_rows();
                this.move_selection(
                    |current, _| current.map_or(0, |i| i.saturating_sub(page)),
                    cx,
                )
            }))
            .on_action(cx.listener(Self::expand))
            .on_action(cx.listener(Self::collapse))
            .on_action(cx.listener(|this, _: &Open, _, cx| {
                if let Some(path) = this.selected.clone() {
                    this.activate(path, true, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &OpenKeepFocus, _, cx| {
                if let Some(path) = this.selected.clone() {
                    this.activate(path, false, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &NewFile, window, cx| {
                this.new_entry(EntryKind::File, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NewFolder, window, cx| {
                this.new_entry(EntryKind::Dir, window, cx)
            }))
            .on_action(cx.listener(|this, _: &Rename, window, cx| {
                if let Some(path) = this.selected.clone() {
                    this.start_rename(path, window, cx)
                }
            }))
            .on_action(cx.listener(Self::duplicate))
            .on_action(cx.listener(Self::move_to_trash))
            .on_action(cx.listener(|this, _: &Cut, _, cx| this.set_clipboard(true, cx)))
            .on_action(cx.listener(|this, _: &Copy, _, cx| this.set_clipboard(false, cx)))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(|this, _: &CopyPath, _, cx| this.copy_path(false, cx)))
            .on_action(cx.listener(|this, _: &CopyRelativePath, _, cx| this.copy_path(true, cx)))
            .on_action(cx.listener(Self::reveal_in_finder))
            .on_action(cx.listener(Self::collapse_all))
            .on_action(cx.listener(Self::show_context_menu))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::confirm_edit))
            .on_action(
                cx.listener(|this, _: &CancelEdit, window, cx| this.cancel_edit(true, window, cx)),
            )
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<DraggedEntry>, _, cx| {
                    this.drag_over(event, cx)
                }),
            )
            .on_drop(cx.listener(|this, entry: &DraggedEntry, window, cx| {
                this.drop_entry(entry, window, cx)
            }))
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<DraggedEdge>, _, cx| {
                    // The handle sits in the gap to the right of the edge: the panel edge follows
                    // the mouse without jumping.
                    let width = f32::from(event.event.position.x - event.bounds.left())
                        - RESIZE_HANDLE_OFFSET;
                    this.width.set(width);
                    this.resizing = true;
                    cx.notify();
                }),
            )
            .child(self.render_header(window, cx))
            .child(
                div()
                    .id("file-tree-list")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    // The insets are on the wrapper: row geometry (`list_index_at`) is computed
                    // from the bounds of the list itself.
                    .pt_0p5()
                    .pb_2()
                    .when(dropping_root, |list| {
                        list.child(drop_overlay(ui).top_0().bottom(px(ROW_INSET)))
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, _| window.focus(&this.focus_handle)),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, event: &MouseDownEvent, window, cx| {
                            this.secondary_click(None, event.position, window, cx)
                        }),
                    )
                    .child(list),
            )
            .child(resize_handle(self.resizing, ui))
            .children(
                self.menu
                    .as_ref()
                    .map(|menu| ContextMenu::overlay(&menu.menu, menu.position)),
            )
    }
}

impl Render for DraggedEntry {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let file = if self.is_dir {
            folder_icon(false, &ui)
        } else {
            file_icon(&self.name, &ui)
        };
        div()
            .flex()
            .items_center()
            .gap_1p5()
            .h(px(ROW_HEIGHT))
            .pl_2()
            .pr_2p5()
            .rounded(px(RADIUS_MD))
            // Opaque: the box floats over the rows, and their text must not show through.
            .bg(UiColors::tint(ui.elevated, 1.))
            .border_1()
            .border_color(ui.elevated_border)
            .shadow(ui::popover_shadow(ui))
            .font_family(theme::UI_FONT)
            .text_size(px(TEXT_SIZE))
            .text_color(ui.foreground)
            .child(file.render().size(px(14.)))
            .child(self.name.clone())
    }
}

impl Render for DraggedEdge {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

/// A full-width list row inset from the island's edges; the highlight belongs to the box inside it.
fn row_shell(list_index: usize) -> Stateful<Div> {
    div()
        .id(list_index)
        .group(ROW_GROUP)
        .h(px(ROW_HEIGHT))
        .w_full()
        .px(px(ROW_INSET))
        .whitespace_nowrap()
}

/// Row box: indent guides, chevron (an empty column for a file), icon. The caller adds the name or
/// the edit field.
fn row_body(depth: usize, kind: EntryKind, expanded: bool, file: FileIcon, ui: UiColors) -> Div {
    div()
        .size_full()
        .flex()
        .items_center()
        .pr_2()
        .rounded(px(RADIUS_SM))
        .child(indent_guides(depth, ui))
        .child(chevron(kind, expanded, ui))
        .child(file.render().ml(px(ICON_GAP)))
}

/// Nesting indent with thin guides: one line per ancestor level, in the middle of the chevron
/// column of that level's directory.
fn indent_guides(depth: usize, ui: UiColors) -> impl IntoElement {
    div()
        .flex_none()
        .h_full()
        .pl(px(ROW_PADDING))
        .flex()
        .children((0..depth).map(move |_| {
            div()
                .flex_none()
                .w(px(INDENT))
                .h_full()
                .pl(px(CHEVRON_WIDTH / 2. - 0.5))
                .child(div().w(px(1.)).h_full().bg(ui.divider))
        }))
}

/// Directory chevron (pointing right when collapsed, down when expanded); for a file, an empty
/// column of the same width.
fn chevron(kind: EntryKind, expanded: bool, ui: UiColors) -> impl IntoElement {
    let glyph = match (kind, expanded) {
        (EntryKind::Dir, true) => Some(IconName::ChevronDown),
        (EntryKind::Dir, false) => Some(IconName::ChevronRight),
        (EntryKind::File, _) => None,
    };
    div()
        .flex_none()
        .w(px(CHEVRON_WIDTH))
        .h_full()
        .flex()
        .items_center()
        .justify_center()
        .children(glyph.map(|glyph| icon(glyph, ui.dim).size(px(CHEVRON_SIZE))))
}

/// Highlight for the root as a drop target: a rounded box inset from the island's edges.
fn drop_overlay(ui: UiColors) -> Div {
    div()
        .absolute()
        .left(px(ROW_INSET))
        .right(px(ROW_INSET))
        .rounded(px(RADIUS_SM))
        .bg(ui.drop_target)
}

/// Width handle in the gap between islands: an accent line on hover and while it is being dragged.
fn resize_handle(resizing: bool, ui: UiColors) -> impl IntoElement {
    div()
        .id("file-tree-resize")
        .group("file-tree-resize")
        .absolute()
        .top_0()
        .bottom_0()
        .right(px(-(1. + RESIZE_HANDLE_WIDTH)))
        .w(px(RESIZE_HANDLE_WIDTH))
        .py(px(ui::RADIUS_LG))
        .flex()
        .justify_center()
        .cursor(CursorStyle::ResizeLeftRight)
        .on_drag(DraggedEdge, |_, _, _, cx| cx.new(|_| DraggedEdge))
        .child(
            div()
                .w(px(2.))
                .h_full()
                .rounded(px(1.))
                .bg(ui.focus_border)
                .when(!resizing, |line| {
                    line.invisible()
                        .group_hover("file-tree-resize", |style| style.visible())
                }),
        )
}

// --- Pure logic ---

fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// What to select when renaming: the name without its extension (as in Finder); for a directory, or
/// for a name that starts with a dot (`.gitignore`), the whole name. In characters.
fn stem_range(name: &str, is_dir: bool) -> Range<usize> {
    let len = name.chars().count();
    match name.rfind('.') {
        Some(dot) if dot > 0 && !is_dir => 0..name[..dot].chars().count(),
        _ => 0..len,
    }
}

/// Validates a name before the operation. The new name may be a path: `src/new/mod.rs` also creates
/// the directories, so every part is checked.
fn check_name(target: &EditTarget, name: &str) -> Result<(), String> {
    let check = |part: &str| validate_name(part).map_err(|error| error.to_string());
    match target {
        EditTarget::Rename { .. } => check(name),
        EditTarget::New { .. } => {
            if name.trim_matches('/').is_empty() {
                return check("");
            }
            name.split('/')
                .filter(|part| !part.is_empty())
                .try_for_each(check)
        }
    }
}

/// The directory to drop `dragged` into when the cursor is over the row `over` (`None` is empty
/// space or the title bar: the root): for a directory, itself; for a file, its directory. `None`
/// means nowhere to drop: onto itself, into its own subtree, or where it already is.
fn drop_dir(tree: &FileTree, dragged: &Path, over: Option<&Path>) -> Option<PathBuf> {
    let dir = tree.target_dir(over);
    (!dir.starts_with(dragged) && dragged.parent() != Some(dir.as_path())).then_some(dir)
}

/// Where the name starts in a row at nesting depth `depth`, measured from the left edge of the
/// list: the ⇧F10 menu is placed under the selected row's name.
fn name_offset(depth: usize) -> f32 {
    ROW_INSET
        + ROW_PADDING
        + depth as f32 * INDENT
        + CHEVRON_WIDTH
        + ICON_GAP
        + ICON_SIZE
        + NAME_GAP
}

/// Row `index` inside the directory `target` where the dragged item will be dropped: the highlight
/// runs as a single band from the directory's row to its last visible descendant. `(top, bottom)`
/// means the row starts or ends the band (the corners are rounded there); `None` means the row is
/// outside the band.
fn drop_band(rows: &[Row], target: &Path, index: usize) -> Option<(bool, bool)> {
    let row = rows.get(index)?;
    if !row.path.starts_with(target) {
        return None;
    }
    let top = row.path == target;
    let bottom = !rows
        .get(index + 1)
        .is_some_and(|next| next.path.starts_with(target));
    Some((top, bottom))
}

/// Path relative to the root, with `/` separators; the root itself is `.`.
fn relative_path(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rest) if rest.as_os_str().is_empty() => ".".into(),
        Ok(rest) => rest
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
        Err(_) => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_selects_the_name_without_extension() {
        assert_eq!(stem_range("main.rs", false), 0..4);
        assert_eq!(stem_range("archive.tar.gz", false), 0..11);
        assert_eq!(stem_range(".gitignore", false), 0..10);
        assert_eq!(stem_range("Makefile", false), 0..8);
        assert_eq!(stem_range("заметки.md", false), 0..7);
        assert_eq!(stem_range("v1.2", true), 0..4);
    }

    #[test]
    fn new_names_may_be_paths_but_every_part_is_checked() {
        let new = EditTarget::New {
            dir: PathBuf::from("/p"),
            kind: EntryKind::File,
        };
        let rename = EditTarget::Rename {
            path: PathBuf::from("/p/a.rs"),
        };
        assert!(check_name(&new, "src/new/mod.rs").is_ok());
        assert!(check_name(&new, "/").is_err());
        assert!(check_name(&new, "").is_err());
        assert!(check_name(&new, "a/../b").is_err());
        assert!(check_name(&rename, "b.rs").is_ok());
        assert!(check_name(&rename, "dir/b.rs").is_err());
    }

    fn tree() -> FileTree {
        let entry = |name: &str, kind| DirEntry {
            name: name.into(),
            kind,
            ignored: false,
        };
        let mut tree = FileTree::new(PathBuf::from("/p"));
        tree.set_listing(
            Path::new("/p"),
            vec![entry("src", EntryKind::Dir), entry("a.rs", EntryKind::File)],
        );
        tree.expand(Path::new("/p/src"));
        tree.set_listing(
            Path::new("/p/src"),
            vec![
                entry("app", EntryKind::Dir),
                entry("main.rs", EntryKind::File),
            ],
        );
        tree
    }

    #[test]
    fn drop_goes_into_dirs_and_next_to_files() {
        let tree = tree();
        let p = |path: &str| PathBuf::from(path);
        // A file from the root: into a directory, and onto a file inside it.
        assert_eq!(
            drop_dir(&tree, &p("/p/a.rs"), Some(&p("/p/src"))),
            Some(p("/p/src"))
        );
        assert_eq!(
            drop_dir(&tree, &p("/p/a.rs"), Some(&p("/p/src/main.rs"))),
            Some(p("/p/src"))
        );
        // Where it already lives, there is nowhere to drop; on empty space, the root.
        assert_eq!(drop_dir(&tree, &p("/p/a.rs"), None), None);
        assert_eq!(drop_dir(&tree, &p("/p/src/main.rs"), None), Some(p("/p")));
        // A directory can't go into itself or into its own subtree.
        assert_eq!(drop_dir(&tree, &p("/p/src"), Some(&p("/p/src"))), None);
        assert_eq!(drop_dir(&tree, &p("/p/src"), Some(&p("/p/src/app"))), None);
        assert_eq!(
            drop_dir(&tree, &p("/p/src/app"), Some(&p("/p/src/main.rs"))),
            None
        );
    }

    #[test]
    fn drop_band_spans_the_dir_and_its_visible_children() {
        let rows = tree().rows();
        let paths: Vec<_> = rows.iter().map(|row| row.path.clone()).collect();
        assert_eq!(
            paths,
            [
                PathBuf::from("/p/src"),
                PathBuf::from("/p/src/app"),
                PathBuf::from("/p/src/main.rs"),
                PathBuf::from("/p/a.rs"),
            ]
        );
        let band = |target: &str, index| drop_band(&rows, Path::new(target), index);
        // A directory with children: the top is the directory itself, the bottom is the last child,
        // and a neighbor is outside the band.
        assert_eq!(band("/p/src", 0), Some((true, false)));
        assert_eq!(band("/p/src", 1), Some((false, false)));
        assert_eq!(band("/p/src", 2), Some((false, true)));
        assert_eq!(band("/p/src", 3), None);
        // A collapsed (or empty) directory is a band of a single row.
        assert_eq!(band("/p/src/app", 1), Some((true, true)));
        // A similar name is not a descendant: `/p/src2` doesn't start with `/p/src` when compared
        // by components.
        assert_eq!(drop_band(&rows, Path::new("/p/sr"), 0), None);
        assert_eq!(band("/p/src", 9), None);
    }

    #[test]
    fn names_line_up_with_nesting() {
        assert_eq!(name_offset(1) - name_offset(0), INDENT);
        // The name is to the right of the icon, and the icon is to the right of the chevron column.
        assert!(name_offset(0) > ROW_INSET + ROW_PADDING + CHEVRON_WIDTH + ICON_SIZE);
    }

    #[test]
    fn relative_paths_use_slashes() {
        let root = Path::new("/p");
        assert_eq!(
            relative_path(root, Path::new("/p/src/main.rs")),
            "src/main.rs"
        );
        assert_eq!(relative_path(root, root), ".");
        assert_eq!(relative_path(root, Path::new("/q/x")), "/q/x");
    }
}
