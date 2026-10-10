//! The commit window: the left island in place of the project tree (⌘0, ⌘K), as the Commit tool
//! window of JetBrains IDEs.
//!
//! - The changed files of every repository: "Changes" and "Unversioned Files" (with several
//!   repositories — under a node per repository with its branch), grouped by directory (or flat,
//!   with the directory dimmed after the name). Checkboxes decide what the commit takes: a file
//!   wholly, partly (only some of its changes — set in the diff viewer), or not at all; a group or a
//!   directory checks everything inside. What is checked lives in `GitStore`, so the diff viewer and
//!   this window agree.
//! - The keyboard: ↑↓, ←→ (collapse, expand), Space (checkbox), ↵ / ⌘D / double click (diff), F4 /
//!   ⌘↓ (open the file), ⌥⌘Z (rollback), ⇥ (to the message), Esc (back to the editor); the context
//!   menu (right button, ⇧F10).
//! - The message (the code font, history of recent messages), Amend (the last commit's message comes
//!   into an empty field), "Commit" (⌘↵) and "Commit and Push…" (⌥⇧⌘K: the push dialog after).
//!
//! - Two tabs, as in JetBrains IDEs: "Commit" and "Stash" (the stashes — [`crate::stash_panel`]);
//!   each has its toolbar under the tabs.
//! - "Merge Conflicts" first in a repository with conflicted files: no checkboxes (they can't be
//!   committed until resolved), "Resolve" opens the Conflicts dialog, ↵ the merge tool; a banner
//!   says what operation is in progress (merging, rebasing 2/5…) with Continue / Skip / Abort.
//! - "Claude · <session>" changelists (part 9.2, as the changelists of JetBrains IDEs): the files
//!   a Claude session changed leave "Changes" for its group while they have uncommitted changes (a
//!   file two sessions changed belongs to the later one). Their checkboxes commit Claude's work as
//!   any others; ↵ shows the file against its text before Claude, ⌥⌘Z rolls it back to that text;
//!   a commit, a rollback or "Move to Changes" takes the file out of the session's list.
//! - "Generate Commit Message with Claude" (as JetBrains AI Assistant's): the checked changes and
//!   the recent commit subjects go to `claude -p` (Haiku), the answer into the field.
//!
//! A commit first saves the open documents it takes, then commits each repository: a file checked
//! partly goes in as its HEAD version with only the checked changes applied (`flux_git::apply_hunks`).
//! A merge in progress is committed as a whole (every change of its repository, nothing partly,
//! no conflicts left; the message comes from git's MERGE_MSG) — the commit concludes the merge.
//! A rollback asks first, then git puts the files back and the open documents follow (one undoable
//! edit each, saved).

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use flux_core::Rope;
use flux_git::{
    CommitContent, CommitFile, CommitRequest, ConflictKind, ConflictSide, FileStatus, Hunk,
    Operation, RepoState,
};
use gpui::{
    Action, Animation, AnimationExt, AnyElement, App, AsyncApp, ClickEvent, ClipboardItem,
    Context, CursorStyle, DismissEvent, Div, DragMoveEvent, Entity, EntityId, EventEmitter,
    FocusHandle, Focusable, FontWeight, KeyBinding, MouseButton, MouseDownEvent, Pixels, Point,
    Render, ScrollStrategy, SharedString, Subscription, Task, UniformListScrollHandle, Window,
    actions, div, prelude::*, px, uniform_list,
};

use crate::claude::{self, ClaudeStore, ClaudeStoreEvent};
use crate::context_menu::ContextMenu;
use crate::dialog::Dialog;
use crate::editor::{self, Editor};
use crate::git::{self, Change, CheckState, GitEvent, GitStore};
use crate::i18n::{tr, trf, trn};
use crate::notification_center::NotificationGroup;
use crate::notifications::Notification;
use crate::icons::{IconName, file_icon, folder_icon, icon};
use crate::rename::difference;
use crate::stash_panel::{StashPanel, StashPanelEvent, StashSelected};
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, RADIUS_MD, RADIUS_SM};

const HEADER_HEIGHT: f32 = 40.;
/// The toolbar of the active tab, under the tabs.
const TOOLBAR_HEIGHT: f32 = 30.;
const ROW_HEIGHT: f32 = 26.;
/// Rows are inset from the island's edges; the highlight is a rounded box inside the row.
const ROW_INSET: f32 = 6.;
const ROW_PADDING: f32 = 6.;
const INDENT: f32 = 14.;
const CHEVRON_WIDTH: f32 = 16.;
const CHEVRON_SIZE: f32 = 12.;
/// Height of the message field.
const MESSAGE_HEIGHT: f32 = 112.;
/// The width handle sits in the gap to the right of the island, as the tree's does.
const RESIZE_HANDLE_WIDTH: f32 = ui::GAP;
const RESIZE_HANDLE_OFFSET: f32 = 1. + RESIZE_HANDLE_WIDTH / 2.;
/// How many recent messages the history offers.
const HISTORY_LIMIT: usize = 20;
/// A history item shows the first line of a message, shortened to this many characters.
const HISTORY_ITEM_CHARS: usize = 60;
/// How many file names a rollback question lists before "and N more".
const LISTED_FILES: usize = 8;
const ROW_GROUP: &str = "commit-row";
/// What the commit message's request shows Claude: the diff up to this size, then only the names
/// of the rest.
const MESSAGE_DIFF_LIMIT: usize = 40_000;
/// How many recent subjects show Claude the project's style.
const MESSAGE_EXAMPLES: usize = 10;
/// Lines of context around a change in the diff Claude reads (as `git diff`).
const DIFF_CONTEXT: u32 = 3;
/// The model that writes commit messages: quick and cheap.
const MESSAGE_MODEL: &str = "haiku";

// The list of changes (context "CommitChanges"): the message field is outside it, so Space and the
// arrows keep typing there.
actions!(
    commit_panel,
    [
        SelectNext,
        SelectPrevious,
        SelectFirst,
        SelectLast,
        Expand,
        Collapse,
        ToggleChecked,
        ShowDiff,
        JumpToSource,
        Rollback,
        Delete,
        CopyPath,
        AddToGitignore,
        ExpandAll,
        CollapseAll,
        ToggleGroupByDirectory,
        ShowContextMenu,
        FocusMessage,
        Cancel,
        Refresh,
        /// A conflicted file: the merge tool.
        MergeFile,
        /// A conflicted file: our side whole.
        AcceptYours,
        /// A conflicted file: their side whole.
        AcceptTheirs,
        /// The selected files into a stash (the Stash Changes dialog for them).
        StashSelectedFiles,
        /// A file of a Claude changelist: its diff against HEAD (↵ shows it against its text
        /// before Claude).
        ShowDiffWithHead,
        /// The selected files of a Claude changelist go back to "Changes" (the session forgets
        /// them).
        MoveToChanges,
    ]
);

// The message field's tools (context "CommitPanel").
actions!(
    commit_panel,
    [
        /// Claude writes the commit message for the checked changes (a second time: stops it).
        GenerateCommitMessage,
    ]
);

// The tabs of the window.
actions!(commit_panel, [ShowCommitTab, ShowStashTab]);

// The whole window (context "CommitPanel"): also from the message field.
actions!(commit_panel, [CommitChanges, CommitAndPush, ToggleAmend]);

/// Puts a message from the history into the field (by its index in the list).
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = commit_panel, no_json)]
pub struct UseMessage(pub usize);

pub fn init(cx: &mut App) {
    let list = Some("CommitChanges");
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, list),
        KeyBinding::new("up", SelectPrevious, list),
        KeyBinding::new("home", SelectFirst, list),
        KeyBinding::new("end", SelectLast, list),
        KeyBinding::new("right", Expand, list),
        KeyBinding::new("left", Collapse, list),
        KeyBinding::new("space", ToggleChecked, list),
        // As in the Commit tool window of JetBrains IDEs.
        KeyBinding::new("enter", ShowDiff, list),
        KeyBinding::new("cmd-d", ShowDiff, list),
        KeyBinding::new("f4", JumpToSource, list),
        KeyBinding::new("cmd-down", JumpToSource, list),
        KeyBinding::new("alt-cmd-z", Rollback, list),
        KeyBinding::new("cmd-backspace", Delete, list),
        KeyBinding::new("alt-cmd-c", CopyPath, list),
        KeyBinding::new("shift-f10", ShowContextMenu, list),
        KeyBinding::new("tab", FocusMessage, list),
        KeyBinding::new("escape", Cancel, list),
    ]);
    let panel = Some("CommitPanel");
    cx.bind_keys([
        KeyBinding::new("cmd-enter", CommitChanges, panel),
        KeyBinding::new("alt-shift-cmd-k", CommitAndPush, panel),
    ]);
}

/// What the commit window asks the workspace to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitPanelEvent {
    /// Show the diff of a file.
    OpenDiff(PathBuf),
    /// Open a file (F4, Jump to Source).
    OpenFile(PathBuf),
    /// Return focus to the editor (Esc).
    FocusEditor,
    /// "Commit and Push…" committed: open the push dialog.
    Push,
    /// Files are gone from disk (deleted, or added ones rolled back with their copies): their
    /// unmodified tabs close.
    Removed(Vec<PathBuf>),
    /// A file of a Claude changelist against its text before Claude (`None` — Claude created it).
    OpenClaudeDiff {
        path: PathBuf,
        repo: usize,
        original: Option<Arc<str>>,
    },
}

/// The groups of a repository's changes, in this order (the Claude changelists after the
/// conflicts, in the order of the sessions).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum GroupKind {
    /// Conflicted files of a merge, rebase, cherry-pick or unstash.
    Conflicts,
    /// The files a Claude session changed (part 9.2).
    Claude(EntityId),
    Changes,
    Unversioned,
}

impl GroupKind {
    fn of(status: FileStatus) -> Self {
        match status {
            FileStatus::Conflicted => GroupKind::Conflicts,
            FileStatus::Untracked => GroupKind::Unversioned,
            _ => GroupKind::Changes,
        }
    }

    fn label(self) -> &'static str {
        match self {
            GroupKind::Conflicts => tr("Merge Conflicts"),
            GroupKind::Claude(_) => "Claude",
            GroupKind::Changes => tr("Changes"),
            GroupKind::Unversioned => tr("Unversioned Files"),
        }
    }
}

// --- Claude's changelists (part 9.2) ---

/// A file a Claude session changed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ClaudeFile {
    /// As the session knows it (Claude's path).
    pub path: PathBuf,
    /// Canonical, as git's paths are: the key against the changes.
    pub key: PathBuf,
    pub changed_at: SystemTime,
}

/// A Claude session with the files it changed: one changelist.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ClaudeList {
    pub session: EntityId,
    pub title: String,
    pub files: Vec<ClaudeFile>,
}

/// The Claude changelists as the tree uses them: the sessions in order and the session that owns
/// each file — the one that changed it last.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ClaudeGroups {
    pub sessions: Vec<(EntityId, String)>,
    /// A file (canonical, and as Claude wrote it) → its session.
    owners: HashMap<PathBuf, (EntityId, SystemTime)>,
}

impl ClaudeGroups {
    pub fn build(lists: &[ClaudeList]) -> Self {
        let mut owners: HashMap<PathBuf, (EntityId, SystemTime)> = HashMap::new();
        for list in lists {
            for file in &list.files {
                for path in [&file.key, &file.path] {
                    let later = owners
                        .get(path)
                        .is_none_or(|(_, changed_at)| file.changed_at >= *changed_at);
                    if later {
                        owners.insert(path.clone(), (list.session, file.changed_at));
                    }
                }
            }
        }
        ClaudeGroups {
            sessions: lists
                .iter()
                .map(|list| (list.session, list.title.clone()))
                .collect(),
            owners,
        }
    }

    /// The session whose changelist has the change; a conflicted file stays with the conflicts.
    pub fn owner(&self, change: &Change) -> Option<EntityId> {
        if change.status == FileStatus::Conflicted {
            return None;
        }
        self.owners.get(&change.path).map(|(session, _)| *session)
    }

    pub fn title(&self, session: EntityId) -> Option<&str> {
        self.sessions
            .iter()
            .find(|(id, _)| *id == session)
            .map(|(_, title)| title.as_str())
    }

    /// The group of a change.
    fn group_of(&self, change: &Change) -> GroupKind {
        match self.owner(change) {
            Some(session) => GroupKind::Claude(session),
            None => GroupKind::of(change.status),
        }
    }

    /// Every group, in order: the conflicts, the sessions' changelists, the rest.
    fn order(&self) -> Vec<GroupKind> {
        let mut groups = vec![GroupKind::Conflicts];
        groups.extend(
            self.sessions
                .iter()
                .map(|(session, _)| GroupKind::Claude(*session)),
        );
        groups.extend([GroupKind::Changes, GroupKind::Unversioned]);
        groups
    }
}

/// The tabs of the commit window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitTab {
    Commit,
    Stash,
}

/// Whether a change can be checked for a commit: a conflicted file can't, until it is resolved.
fn checkable(change: &Change) -> bool {
    change.status != FileStatus::Conflicted
}

/// What a row is, kept across refreshes: the selection and the collapsed nodes follow it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum RowKey {
    Repo(usize),
    Group(usize, GroupKind),
    /// A directory of a group, by its path relative to the repository.
    Dir(usize, GroupKind, String),
    File(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RowKind {
    Repo {
        name: String,
        branch: Option<String>,
    },
    Group(GroupKind),
    /// A directory (single-child chains are joined: `src/app/ui`).
    Dir {
        label: String,
    },
    /// A file: its name and, in the flat view, its directory.
    File {
        name: String,
        detail: Option<String>,
    },
}

/// A visible row of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Row {
    pub key: RowKey,
    pub depth: usize,
    pub kind: RowKind,
    /// The changes under the row (indices into the change list): the file itself, or everything
    /// inside a node.
    pub files: Vec<usize>,
    pub expanded: bool,
}

impl Row {
    fn expandable(&self) -> bool {
        !matches!(self.kind, RowKind::File { .. })
    }
}

/// A repository for the tree: its folder name and branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepoLabel {
    pub name: String,
    pub branch: Option<String>,
}

/// The visible rows: per repository (a node of its own when there are several) the groups
/// "Changes" and "Unversioned Files" (the conflicts and the Claude changelists before them), then
/// directories (joined through single-child chains) and files in natural order — or files by path,
/// flat. Collapsed nodes hide what is inside.
pub(crate) fn build_rows(
    changes: &[Change],
    repos: &[RepoLabel],
    by_directory: bool,
    collapsed: &HashSet<RowKey>,
    claude: &ClaudeGroups,
) -> Vec<Row> {
    let mut rows = Vec::new();
    let several = repos.len() > 1;
    for (repo, label) in repos.iter().enumerate() {
        let in_repo: Vec<usize> = (0..changes.len())
            .filter(|&index| changes[index].repo == repo)
            .collect();
        if in_repo.is_empty() {
            continue;
        }
        let mut depth = 0;
        if several {
            let key = RowKey::Repo(repo);
            let expanded = !collapsed.contains(&key);
            rows.push(Row {
                key,
                depth,
                kind: RowKind::Repo {
                    name: label.name.clone(),
                    branch: label.branch.clone(),
                },
                files: in_repo.clone(),
                expanded,
            });
            if !expanded {
                continue;
            }
            depth = 1;
        }
        for group in claude.order() {
            let files: Vec<usize> = in_repo
                .iter()
                .copied()
                .filter(|&index| claude.group_of(&changes[index]) == group)
                .collect();
            if files.is_empty() {
                continue;
            }
            let key = RowKey::Group(repo, group);
            let expanded = !collapsed.contains(&key);
            rows.push(Row {
                key,
                depth,
                kind: RowKind::Group(group),
                files: files.clone(),
                expanded,
            });
            if !expanded {
                continue;
            }
            if by_directory {
                let tree = DirTree::build(changes, &files);
                tree.emit(changes, repo, group, "", depth + 1, collapsed, &mut rows);
            } else {
                let mut sorted = files;
                sorted.sort_by(|&a, &b| compare_paths(&changes[a].relative, &changes[b].relative));
                for index in sorted {
                    let (dir, name) = split_relative(&changes[index].relative);
                    rows.push(file_row(
                        changes,
                        index,
                        name,
                        (!dir.is_empty()).then(|| dir.into()),
                        depth + 1,
                    ));
                }
            }
        }
    }
    rows
}

fn file_row(
    changes: &[Change],
    index: usize,
    name: &str,
    detail: Option<String>,
    depth: usize,
) -> Row {
    Row {
        key: RowKey::File(changes[index].path.clone()),
        depth,
        kind: RowKind::File {
            name: name.to_string(),
            detail,
        },
        files: vec![index],
        expanded: false,
    }
}

/// `src/app/main.rs` → (`src/app`, `main.rs`).
fn split_relative(relative: &str) -> (&str, &str) {
    match relative.rsplit_once('/') {
        Some((dir, name)) => (dir, name),
        None => ("", relative),
    }
}

/// Paths in the order of the project tree: by component, natural order.
fn compare_paths(a: &str, b: &str) -> Ordering {
    let mut left = a.split('/');
    let mut right = b.split('/');
    loop {
        match (left.next(), right.next()) {
            (Some(x), Some(y)) => match flux_fs::compare_names(x, y) {
                Ordering::Equal => continue,
                other => return other,
            },
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (None, None) => return Ordering::Equal,
        }
    }
}

/// The directories of a group's files.
#[derive(Default)]
struct DirTree {
    dirs: BTreeMap<String, DirTree>,
    /// Files directly inside, as indices into the change list.
    files: Vec<usize>,
}

impl DirTree {
    fn build(changes: &[Change], files: &[usize]) -> Self {
        let mut root = DirTree::default();
        for &index in files {
            let mut node = &mut root;
            let parts: Vec<&str> = changes[index].relative.split('/').collect();
            for part in &parts[..parts.len() - 1] {
                node = node.dirs.entry(part.to_string()).or_default();
            }
            node.files.push(index);
        }
        root
    }

    /// Every file inside, at any depth.
    fn all_files(&self) -> Vec<usize> {
        let mut files = self.files.clone();
        for dir in self.dirs.values() {
            files.extend(dir.all_files());
        }
        files
    }

    /// Rows for the directories (first, natural order) and the files of this node.
    #[allow(clippy::too_many_arguments)]
    fn emit(
        &self,
        changes: &[Change],
        repo: usize,
        group: GroupKind,
        prefix: &str,
        depth: usize,
        collapsed: &HashSet<RowKey>,
        rows: &mut Vec<Row>,
    ) {
        let mut dirs: Vec<(&String, &DirTree)> = self.dirs.iter().collect();
        dirs.sort_by(|a, b| flux_fs::compare_names(a.0, b.0));
        for (name, dir) in dirs {
            // A chain of directories with a single child directory and no files is one row.
            let mut label = name.clone();
            let mut node = dir;
            while node.files.is_empty() && node.dirs.len() == 1 {
                let (child, next) = node.dirs.iter().next().expect("one child");
                label = format!("{label}/{child}");
                node = next;
            }
            let path = if prefix.is_empty() {
                label.clone()
            } else {
                format!("{prefix}/{label}")
            };
            let key = RowKey::Dir(repo, group, path.clone());
            let expanded = !collapsed.contains(&key);
            rows.push(Row {
                key,
                depth,
                kind: RowKind::Dir { label },
                files: node.all_files(),
                expanded,
            });
            if expanded {
                node.emit(changes, repo, group, &path, depth + 1, collapsed, rows);
            }
        }
        let mut files = self.files.clone();
        files.sort_by(|&a, &b| {
            flux_fs::compare_names(
                split_relative(&changes[a].relative).1,
                split_relative(&changes[b].relative).1,
            )
        });
        for index in files {
            let name = split_relative(&changes[index].relative).1;
            rows.push(file_row(changes, index, name, None, depth));
        }
    }
}

/// The checkbox of a node: all checked, none, or some (a partly checked file counts as some).
pub(crate) fn aggregate(states: impl IntoIterator<Item = CheckState>) -> CheckState {
    let mut checked = false;
    let mut unchecked = false;
    for state in states {
        match state {
            CheckState::Checked => checked = true,
            CheckState::Unchecked => unchecked = true,
            CheckState::Partial => return CheckState::Partial,
        }
    }
    match (checked, unchecked) {
        (true, false) => CheckState::Checked,
        (false, _) => CheckState::Unchecked,
        (true, true) => CheckState::Partial,
    }
}

/// What a partly checked file commits: its HEAD version with only the included changes applied.
/// The texts are compared without `\r` (as the gutter compares them, so the hunks are the same);
/// a file with CRLF line endings gets them back.
pub(crate) fn partial_content(
    base: &str,
    current: &str,
    include: impl Fn(&Hunk) -> bool,
) -> String {
    let crlf = current.contains("\r\n");
    let base = base.replace("\r\n", "\n");
    let current = current.replace("\r\n", "\n");
    let hunks = flux_git::diff_lines(&base, &current);
    let content = flux_git::apply_hunks(&base, &current, &hunks, include);
    if crlf {
        content.replace('\n', "\r\n")
    } else {
        content
    }
}

/// A popup menu of the window: the context menu or the message history.
struct Menu {
    menu: Entity<ContextMenu>,
    position: Point<Pixels>,
    _subscriptions: [Subscription; 2],
}

/// Dragging the island's right edge.
#[derive(Debug, Clone, Copy)]
struct DraggedCommitEdge;

impl Render for DraggedCommitEdge {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

pub struct CommitPanel {
    git: Entity<GitStore>,
    /// The tab shown: the commit or the stashes.
    tab: CommitTab,
    stash: Entity<StashPanel>,
    message: Entity<Editor>,
    /// The message of a merge in progress (git's MERGE_MSG) put into an empty field: it goes away
    /// with the merge, unless it was edited.
    merge_message: Option<String>,
    amend: bool,
    /// The message Amend put into the field: turning Amend off takes it back, if it is unchanged.
    amend_message: Option<String>,
    group_by_directory: bool,
    collapsed: HashSet<RowKey>,
    /// The changes and the visible rows, rebuilt when the git status or the view changes.
    changes: Vec<Change>,
    rows: Vec<Row>,
    selected: Option<RowKey>,
    /// A commit is running: the buttons wait.
    committing: bool,
    /// The list of changes; the message field has its own.
    focus_handle: FocusHandle,
    scroll: UniformListScrollHandle,
    /// Shared with the project tree: the left island keeps its width whichever is shown.
    width: ui::LeftIslandWidth,
    resizing: bool,
    menu: Option<Menu>,
    /// Recent messages, for the history menu ([`UseMessage`] picks by index).
    history: Vec<String>,
    /// Claude Code of the window: its sessions' changelists, the commit message it writes.
    claude: Entity<ClaudeStore>,
    claude_lists: Vec<ClaudeList>,
    claude_groups: ClaudeGroups,
    /// Sessions without a process (resumed, stopped) whose files were checked against git once:
    /// those committed meanwhile left their changelist.
    claude_checked: HashSet<EntityId>,
    /// Claude writes the commit message.
    generating: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<CommitPanelEvent> for CommitPanel {}

impl CommitPanel {
    pub fn new(
        git: Entity<GitStore>,
        claude: Entity<ClaudeStore>,
        width: ui::LeftIslandWidth,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let message = cx.new(|cx| Editor::message(tr("Commit Message"), window, cx));
        let stash = cx.new(|cx| StashPanel::new(git.clone(), window, cx));
        let subscriptions = vec![
            cx.observe(&git, |this, _, cx| this.rebuild(cx)),
            // Not every event: a streaming answer notifies the store with each token.
            cx.subscribe(&claude, |this, _, event: &ClaudeStoreEvent, cx| match event {
                ClaudeStoreEvent::SessionAdded(_)
                | ClaudeStoreEvent::SessionRemoved(_)
                | ClaudeStoreEvent::FilesChanged(_)
                | ClaudeStoreEvent::Changed => this.claude_changed(cx),
                _ => {}
            }),
            // The stash tab's toolbar follows its selection.
            cx.observe(&stash, |_, _, cx| cx.notify()),
            cx.subscribe(&stash, |_, _, event: &StashPanelEvent, cx| match event {
                StashPanelEvent::FocusEditor => cx.emit(CommitPanelEvent::FocusEditor),
            }),
        ];
        let mut panel = Self {
            git,
            tab: CommitTab::Commit,
            stash,
            message,
            merge_message: None,
            amend: false,
            amend_message: None,
            group_by_directory: true,
            collapsed: HashSet::new(),
            changes: Vec::new(),
            rows: Vec::new(),
            selected: None,
            committing: false,
            focus_handle: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            width,
            resizing: false,
            menu: None,
            history: Vec::new(),
            claude,
            claude_lists: Vec::new(),
            claude_groups: ClaudeGroups::default(),
            claude_checked: HashSet::new(),
            generating: None,
            _subscriptions: subscriptions,
        };
        panel.claude_lists = panel.read_claude_lists(cx);
        panel.claude_groups = ClaudeGroups::build(&panel.claude_lists);
        panel.rebuild(cx);
        panel
    }

    /// ⌘K: the Commit tab, focus in the message.
    pub fn focus_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_tab(CommitTab::Commit, cx);
        window.focus(&self.message.focus_handle(cx));
    }

    /// ⌘0: focus in the list of the tab shown (the changes, or the stashes); the first row is
    /// selected if nothing is.
    pub fn focus_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab == CommitTab::Stash {
            return self.stash.update(cx, |stash, cx| stash.focus(window, cx));
        }
        window.focus(&self.focus_handle);
        if self.selected_index().is_none() && !self.rows.is_empty() {
            self.select_row(0, ScrollStrategy::Top, cx);
        }
    }

    /// The Stash tab, focused (Unstash Changes…).
    pub fn show_stash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_tab(CommitTab::Stash, cx);
        self.stash.update(cx, |stash, cx| stash.focus(window, cx));
    }

    /// Switches the tab. Focus inside the hidden tab moves to the one shown (a hidden element that
    /// keeps focus would take the window's keys away).
    fn switch_tab(&mut self, tab: CommitTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab == tab {
            return;
        }
        let focused = self.contains_focus(window, cx);
        self.set_tab(tab, cx);
        if focused {
            self.focus_changes(window, cx);
        }
    }

    fn set_tab(&mut self, tab: CommitTab, cx: &mut Context<Self>) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        self.stash.update(cx, |stash, cx| {
            stash.set_active(tab == CommitTab::Stash, cx)
        });
        cx.notify();
    }

    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus_handle.contains_focused(window, cx)
            || self.message.focus_handle(cx).contains_focused(window, cx)
            || self
                .menu
                .as_ref()
                .is_some_and(|menu| menu.menu.focus_handle(cx).contains_focused(window, cx))
            || self.stash.read(cx).contains_focus(window, cx)
    }

    // --- The tree ---

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let git = self.git.read(cx);
        self.changes = git.changes();
        let repos: Vec<RepoLabel> = git
            .repos()
            .iter()
            .map(|entry| RepoLabel {
                name: entry
                    .repo
                    .work_dir
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                branch: entry.status.branch.label(),
            })
            .collect();
        self.rows = build_rows(
            &self.changes,
            &repos,
            self.group_by_directory,
            &self.collapsed,
            &self.claude_groups,
        );
        self.follow_merge(cx);
        self.check_stopped_sessions(cx);
        // A selected file hidden in a collapsed node: the node is selected; one that went away
        // (committed, rolled back): nothing is.
        if self.selected_index().is_none() {
            self.selected = match self.selected.take() {
                Some(RowKey::File(path)) => self
                    .changes
                    .iter()
                    .position(|change| change.path == path)
                    .and_then(|index| {
                        self.rows
                            .iter()
                            .filter(|row| row.files.contains(&index))
                            .max_by_key(|row| row.depth)
                    })
                    .map(|row| row.key.clone()),
                _ => None,
            };
        }
        cx.notify();
    }

    // --- Claude's changelists ---

    /// The files each session of the window changed (none while Claude Code is off).
    fn read_claude_lists(&self, cx: &App) -> Vec<ClaudeList> {
        if !claude::enabled(cx) {
            return Vec::new();
        }
        self.claude
            .read(cx)
            .sessions()
            .iter()
            .filter_map(|session| {
                let model = session.read(cx);
                let files: Vec<ClaudeFile> = model
                    .model()
                    .changed_files
                    .iter()
                    .map(|file| ClaudeFile {
                        path: file.path.clone(),
                        key: crate::navigation::canonical(&file.path),
                        changed_at: file.changed_at,
                    })
                    .collect();
                (!files.is_empty()).then(|| ClaudeList {
                    session: session.entity_id(),
                    title: model.title().to_string(),
                    files,
                })
            })
            .collect()
    }

    /// A session came or went, changed files or its title: the tree follows when the changelists
    /// differ.
    fn claude_changed(&mut self, cx: &mut Context<Self>) {
        let lists = self.read_claude_lists(cx);
        let alive: HashSet<EntityId> = self
            .claude
            .read(cx)
            .sessions()
            .iter()
            .map(|session| session.entity_id())
            .collect();
        self.claude_checked.retain(|session| alive.contains(session));
        if lists != self.claude_lists {
            self.claude_lists = lists;
            self.claude_groups = ClaudeGroups::build(&self.claude_lists);
            self.rebuild(cx);
        } else {
            // The generate button follows `claude`'s state.
            cx.notify();
        }
    }

    /// A session that came without a process (resumed after a restart) may list files committed
    /// since Claude changed them: once git's status is in, those leave its changelist. A session
    /// seen running isn't checked — its newest edit may not be in the status yet.
    fn check_stopped_sessions(&mut self, cx: &mut Context<Self>) {
        let ready = {
            let git = self.git.read(cx);
            !git.is_discovering()
                && git
                    .repos()
                    .iter()
                    .all(|repo| repo.status.branch != Default::default())
        };
        if !ready || self.claude_lists.is_empty() {
            return;
        }
        let changed: HashSet<&Path> = self
            .changes
            .iter()
            .map(|change| change.path.as_path())
            .collect();
        let mut forget: Vec<(EntityId, Vec<PathBuf>)> = Vec::new();
        for list in &self.claude_lists {
            if self.claude_checked.contains(&list.session) {
                continue;
            }
            let Some(session) = self.claude.read(cx).session(list.session).cloned() else {
                continue;
            };
            let session = session.read(cx);
            if session.is_loading() {
                continue;
            }
            self.claude_checked.insert(list.session);
            if session.is_started() {
                continue;
            }
            let committed: Vec<PathBuf> = list
                .files
                .iter()
                .filter(|file| {
                    !changed.contains(file.key.as_path()) && !changed.contains(file.path.as_path())
                })
                .map(|file| file.path.clone())
                .collect();
            if !committed.is_empty() {
                forget.push((list.session, committed));
            }
        }
        for (session, paths) in forget {
            self.forget_claude_files(session, &paths, cx);
        }
    }

    /// The files leave a session's changelist (committed, rolled back, moved to "Changes").
    fn forget_claude_files(&mut self, session: EntityId, paths: &[PathBuf], cx: &mut Context<Self>) {
        if let Some(session) = self.claude.read(cx).session(session).cloned() {
            session.update(cx, |session, cx| session.forget_changed_files(paths, cx));
        }
    }

    /// The changelist file behind a change: its session and the path as Claude wrote it.
    fn claude_file(&self, change: &Change) -> Option<(EntityId, PathBuf)> {
        let session = self.claude_groups.owner(change)?;
        let list = self.claude_lists.iter().find(|list| list.session == session)?;
        let file = list
            .files
            .iter()
            .find(|file| file.key == change.path || file.path == change.path)?;
        Some((session, file.path.clone()))
    }

    /// A change's text before Claude: `Some(None)` — Claude created the file; `None` — not a file
    /// of a changelist.
    fn claude_original(&self, change: &Change, cx: &App) -> Option<Option<Arc<str>>> {
        let (session, path) = self.claude_file(change)?;
        let session = self.claude.read(cx).session(session)?.read(cx);
        let file = session.model().changed_file(&path)?;
        Some(file.original.as_deref().map(Arc::from))
    }

    /// Whether the selected row is in a Claude changelist (the session).
    fn selected_claude_session(&self) -> Option<EntityId> {
        match &self.selected_row()?.key {
            RowKey::Group(_, GroupKind::Claude(session))
            | RowKey::Dir(_, GroupKind::Claude(session), _) => Some(*session),
            RowKey::File(_) => {
                let change = self.selected_file()?;
                self.claude_groups.owner(change)
            }
            _ => None,
        }
    }

    /// A merge in progress puts git's message into an empty field (and Amend can't go with it);
    /// when the merge is over, an unedited message goes away.
    fn follow_merge(&mut self, cx: &mut Context<Self>) {
        let merge_message = {
            let git = self.git.read(cx);
            git.repos_in_progress().into_iter().find_map(|repo| {
                let operation = git.operation(repo);
                (operation.state == RepoState::Merging)
                    .then(|| operation.message.clone().unwrap_or_default())
            })
        };
        match merge_message {
            Some(message) => {
                if self.amend {
                    self.amend = false;
                    if self.amend_message.take().as_deref() == Some(self.message_text(cx).as_str())
                    {
                        self.set_message("", cx);
                    }
                }
                if self.merge_message.is_none()
                    && !message.is_empty()
                    && self.message_text(cx).trim().is_empty()
                {
                    self.set_message(&message, cx);
                    self.merge_message = Some(message);
                }
            }
            None => {
                if let Some(message) = self.merge_message.take()
                    && self.message_text(cx) == message
                {
                    self.set_message("", cx);
                }
            }
        }
    }

    /// Repositories in the middle of a merge: their commit concludes it.
    fn merging_repos(&self, cx: &App) -> Vec<usize> {
        let git = self.git.read(cx);
        git.repos_in_progress()
            .into_iter()
            .filter(|&repo| git.operation(repo).state == RepoState::Merging)
            .collect()
    }

    fn selected_index(&self) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        self.rows.iter().position(|row| row.key == *selected)
    }

    fn selected_row(&self) -> Option<&Row> {
        self.selected_index().map(|index| &self.rows[index])
    }

    fn select_row(&mut self, index: usize, strategy: ScrollStrategy, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index) else {
            return;
        };
        self.selected = Some(row.key.clone());
        self.scroll.scroll_to_item(index, strategy);
        cx.notify();
    }

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

    fn set_expanded(&mut self, key: &RowKey, expanded: bool, cx: &mut Context<Self>) {
        if expanded {
            self.collapsed.remove(key);
        } else {
            self.collapsed.insert(key.clone());
        }
        self.rebuild(cx);
    }

    /// → : expands a collapsed node; on an expanded one, goes to its first child.
    fn expand(&mut self, _: &Expand, _: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected_index() else {
            return;
        };
        let row = self.rows[index].clone();
        if !row.expandable() {
            return;
        }
        if row.expanded {
            self.select_row(index + 1, ScrollStrategy::Bottom, cx);
        } else {
            self.set_expanded(&row.key, true, cx);
        }
    }

    /// ← : collapses an expanded node; otherwise goes to the parent.
    fn collapse(&mut self, _: &Collapse, _: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected_index() else {
            return;
        };
        let row = self.rows[index].clone();
        if row.expandable() && row.expanded {
            return self.set_expanded(&row.key, false, cx);
        }
        if let Some(parent) = self.rows[..index].iter().rposition(|r| r.depth < row.depth) {
            self.select_row(parent, ScrollStrategy::Top, cx);
        }
    }

    /// Expand All, or Collapse All down to the groups (and repositories).
    fn set_all_expanded(&mut self, expanded: bool, cx: &mut Context<Self>) {
        self.collapsed.clear();
        self.rebuild(cx);
        if !expanded {
            let nodes: Vec<RowKey> = self
                .rows
                .iter()
                .filter(|row| matches!(row.kind, RowKind::Dir { .. }))
                .map(|row| row.key.clone())
                .collect();
            self.collapsed.extend(nodes);
            self.rebuild(cx);
        }
    }

    // --- Checkboxes ---

    fn row_state(&self, row: &Row, cx: &App) -> CheckState {
        let git = self.git.read(cx);
        aggregate(
            row.files
                .iter()
                .filter_map(|&index| self.changes.get(index))
                .filter(|change| checkable(change))
                .map(|change| git.check_state(change)),
        )
    }

    /// Whether a row has a checkbox: something under it can go into a commit.
    fn row_checkable(&self, row: &Row) -> bool {
        row.files
            .iter()
            .filter_map(|&index| self.changes.get(index))
            .any(checkable)
    }

    /// Clicking a checkbox: a checked node is unchecked, anything else is checked wholly.
    fn toggle_row(&mut self, key: &RowKey, cx: &mut Context<Self>) {
        let Some(row) = self.rows.iter().find(|row| row.key == *key).cloned() else {
            return;
        };
        let include = self.row_state(&row, cx) != CheckState::Checked;
        let paths: Vec<PathBuf> = row
            .files
            .iter()
            .filter_map(|&index| self.changes.get(index))
            .filter(|change| checkable(change))
            .map(|change| change.path.clone())
            .collect();
        self.git.update(cx, |git, cx| {
            for path in &paths {
                git.set_included(path, include, cx);
            }
        });
    }

    fn toggle_checked(&mut self, _: &ToggleChecked, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(key) = self.selected.clone() {
            self.toggle_row(&key, cx);
        }
    }

    // --- Operations on the selection ---

    /// The changes the operations act on: under the selected row; with nothing selected, the
    /// checked ones.
    fn target_changes(&self, cx: &App) -> Vec<Change> {
        match self.selected_row() {
            Some(row) => row
                .files
                .iter()
                .filter_map(|&index| self.changes.get(index).cloned())
                .collect(),
            None => {
                let git = self.git.read(cx);
                self.changes
                    .iter()
                    .filter(|change| checkable(change))
                    .filter(|change| git.check_state(change) != CheckState::Unchecked)
                    .cloned()
                    .collect()
            }
        }
    }

    fn selected_file(&self) -> Option<&Change> {
        match self.selected_row()? {
            Row {
                kind: RowKind::File { .. },
                files,
                ..
            } => self.changes.get(*files.first()?),
            _ => None,
        }
    }

    /// ↵ / ⌘D: the diff of the selected file (a conflicted one: the merge tool); on a node, ↵
    /// expands or collapses it.
    fn show_diff(&mut self, _: &ShowDiff, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(change) = self.selected_file().cloned() {
            if change.status == FileStatus::Conflicted {
                return open_conflict(&change, window, cx);
            }
            return self.open_diff_of(&change, cx);
        }
        if let Some(row) = self.selected_row().cloned()
            && row.expandable()
        {
            self.set_expanded(&row.key, !row.expanded, cx);
        }
    }

    /// The diff of a change: in a Claude changelist against its text before Claude, otherwise
    /// against HEAD.
    fn open_diff_of(&mut self, change: &Change, cx: &mut Context<Self>) {
        match self.claude_original(change, cx) {
            Some(original) => cx.emit(CommitPanelEvent::OpenClaudeDiff {
                path: change.path.clone(),
                repo: change.repo,
                original,
            }),
            None => cx.emit(CommitPanelEvent::OpenDiff(change.path.clone())),
        }
    }

    /// A file of a Claude changelist against HEAD, as the other files.
    fn show_diff_with_head(&mut self, _: &ShowDiffWithHead, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(change) = self.selected_file().cloned() {
            cx.emit(CommitPanelEvent::OpenDiff(change.path));
        }
    }

    /// F4: the selected file in its tab.
    fn jump_to_source(&mut self, _: &JumpToSource, _: &mut Window, cx: &mut Context<Self>) {
        let Some(change) = self.selected_file().cloned() else {
            return;
        };
        if change.status == FileStatus::Deleted {
            let message = trf("“{0}” is deleted", &[&file_name(&change.path)]);
            return self.report(GitEvent::Message(message.into()), cx);
        }
        cx.emit(CommitPanelEvent::OpenFile(change.path));
    }

    /// ⌥⌘Z: rolls back the selected changes (the checked ones, with nothing selected), after a
    /// question. Conflicted files are resolved, not rolled back.
    fn rollback(&mut self, _: &Rollback, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_claude_session().is_some() {
            return self.rollback_claude(window, cx);
        }
        let changes: Vec<Change> = self
            .target_changes(cx)
            .into_iter()
            .filter(checkable)
            .collect();
        let this = cx.weak_entity();
        confirm_rollback(self.git.clone(), changes, window, cx, move |removed, cx| {
            this.update(cx, |_, cx| cx.emit(CommitPanelEvent::Removed(removed)))
                .ok();
        });
    }

    /// The selected files of a Claude changelist: each with its session, Claude's path and its
    /// text before Claude.
    fn selected_claude_files(&self, cx: &App) -> Vec<ClaudeTarget> {
        let Some(row) = self.selected_row() else {
            return Vec::new();
        };
        row.files
            .iter()
            .filter_map(|&index| self.changes.get(index))
            .filter_map(|change| {
                let (session, path) = self.claude_file(change)?;
                let original = self.claude_original(change, cx)?;
                Some(ClaudeTarget {
                    change: change.clone(),
                    session,
                    path,
                    original,
                })
            })
            .collect()
    }

    /// ⌥⌘Z in a Claude changelist: the selected files get their text before Claude (a file Claude
    /// created goes to the Trash), after a question; open documents follow (one undoable edit,
    /// saved) and the files leave the changelist.
    fn rollback_claude(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let targets = self.selected_claude_files(cx);
        if targets.is_empty() {
            return;
        }
        let created = targets.iter().any(|target| target.original.is_none());
        let question = match targets.as_slice() {
            [target] => trf(
                "Roll back Claude's changes in “{0}”?",
                &[&file_name(&target.change.path)],
            ),
            _ => trf(
                "Roll back Claude's changes {0}?",
                &[&trn(targets.len(), "in {n} file", "in {n} files")],
            ),
        };
        let mut message = tr("The files get back their text from before Claude changed them.").to_string();
        if created {
            message.push(' ');
            message.push_str(tr("Files Claude created are moved to the Trash."));
        }
        let mut files = targets
            .iter()
            .take(LISTED_FILES)
            .map(|target| target.change.relative.clone())
            .collect::<Vec<_>>()
            .join("\n");
        if targets.len() > LISTED_FILES {
            files.push('\n');
            files.push_str(&trf("and {0} more", &[&(targets.len() - LISTED_FILES)]));
        }
        let answer = Dialog::warning(question)
            .message(message)
            .details(files)
            .danger(tr("Rollback"))
            .cancel(tr("Cancel"))
            .show(window, cx);
        cx.spawn(async move |this, cx| {
            if answer.await != Some(0) {
                return;
            }
            let jobs: Vec<(PathBuf, Option<Arc<str>>)> = targets
                .iter()
                .map(|target| (target.change.path.clone(), target.original.clone()))
                .collect();
            let errors = cx
                .background_spawn(async move { restore_originals(&jobs) })
                .await;
            this.update(cx, |this, cx| {
                let changes: Vec<Change> =
                    targets.iter().map(|target| target.change.clone()).collect();
                let removed = follow_rollback(&this.git, &changes, cx);
                let mut by_session: Vec<(EntityId, Vec<PathBuf>)> = Vec::new();
                for target in &targets {
                    match by_session.iter_mut().find(|(id, _)| *id == target.session) {
                        Some((_, paths)) => paths.push(target.path.clone()),
                        None => by_session.push((target.session, vec![target.path.clone()])),
                    }
                }
                for (session, paths) in by_session {
                    this.forget_claude_files(session, &paths, cx);
                }
                this.git.update(cx, |git, cx| git.refresh(cx));
                let notification = if errors.is_empty() {
                    Notification::success(match changes.as_slice() {
                        [change] => trf(
                            "Rolled back Claude's changes: {0}",
                            &[&file_name(&change.path)],
                        ),
                        _ => trf(
                            "Rolled back Claude's changes in {0}",
                            &[&trn(changes.len(), "{n} file", "{n} files")],
                        ),
                    })
                } else {
                    Notification::error(tr("Couldn't roll back Claude's changes"))
                        .body(errors.join("\n"))
                };
                this.git.update(cx, |git, cx| git.notify(notification, cx));
                if !removed.is_empty() {
                    cx.emit(CommitPanelEvent::Removed(removed));
                }
            })
            .ok();
        })
        .detach();
    }

    /// "Move to Changes": the selected files leave their Claude changelist.
    fn move_to_changes(&mut self, _: &MoveToChanges, _: &mut Window, cx: &mut Context<Self>) {
        let targets = self.selected_claude_files(cx);
        let mut by_session: Vec<(EntityId, Vec<PathBuf>)> = Vec::new();
        for target in targets {
            match by_session.iter_mut().find(|(id, _)| *id == target.session) {
                Some((_, paths)) => paths.push(target.path),
                None => by_session.push((target.session, vec![target.path])),
            }
        }
        for (session, paths) in by_session {
            self.forget_claude_files(session, &paths, cx);
        }
    }

    /// ⌘⌫ on unversioned files: to the Trash, after a question. Tracked files are rolled back
    /// instead (as in JetBrains IDEs, Delete doesn't touch history).
    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        let paths: Vec<PathBuf> = self
            .target_changes(cx)
            .into_iter()
            .filter(|change| change.status == FileStatus::Untracked)
            .map(|change| change.path)
            .collect();
        if paths.is_empty() {
            return;
        }
        let question = match paths.as_slice() {
            [path] => trf("Move “{0}” to Trash?", &[&file_name(path)]),
            _ => trf(
                "Move {0} to Trash?",
                &[&trn(paths.len(), "{n} file", "{n} files")],
            ),
        };
        let answer = Dialog::warning(question)
            .message(tr("You can restore it from the Trash."))
            .danger(tr("Move to Trash"))
            .cancel(tr("Cancel"))
            .show(window, cx);
        cx.spawn(async move |this, cx| {
            if answer.await != Some(0) {
                return;
            }
            let trashed = paths.clone();
            let result = cx
                .background_spawn(async move { flux_fs::trash(&trashed) })
                .await;
            this.update(cx, |this, cx| match result {
                Ok(()) => {
                    this.git.update(cx, |git, cx| git.refresh(cx));
                    cx.emit(CommitPanelEvent::Removed(paths));
                }
                Err(err) => {
                    let notification = Notification::error(tr("Couldn't move to Trash"))
                        .body(err.to_string())
                        .group(NotificationGroup::Files);
                    this.git.update(cx, |git, cx| git.notify(notification, cx))
                }
            })
            .ok();
        })
        .detach();
    }

    /// The selected file's (or node's) absolute path to the clipboard.
    fn copy_path(&mut self, _: &CopyPath, _: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.selected_row() else {
            return;
        };
        let path = match &row.key {
            RowKey::File(path) => Some(path.clone()),
            RowKey::Dir(repo, _, relative) => {
                self.repo_dir(*repo, cx).map(|dir| dir.join(relative))
            }
            RowKey::Group(repo, _) | RowKey::Repo(repo) => self.repo_dir(*repo, cx),
        };
        if let Some(path) = path {
            cx.write_to_clipboard(ClipboardItem::new_string(path.display().to_string()));
        }
    }

    fn repo_dir(&self, repo: usize, cx: &App) -> Option<PathBuf> {
        let git = self.git.read(cx);
        git.repos()
            .get(repo)
            .map(|entry| entry.repo.work_dir.clone())
    }

    /// Adds the selected unversioned file, or directory, to `.gitignore`.
    fn add_to_gitignore(&mut self, _: &AddToGitignore, _: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.selected_row().cloned() else {
            return;
        };
        let path = match &row.key {
            RowKey::File(path) => path.clone(),
            RowKey::Dir(repo, GroupKind::Unversioned, relative) => {
                let Some(dir) = self.repo_dir(*repo, cx) else {
                    return;
                };
                dir.join(relative)
            }
            _ => return,
        };
        let name = file_name(&path);
        let task = self
            .git
            .update(cx, |git, cx| git.add_to_gitignore(&path, cx));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let notification = match result {
                    Ok(()) => Notification::success(trf("Added to .gitignore: {0}", &[&name])),
                    Err(err) => Notification::error(tr("Couldn't add to .gitignore")).body(err),
                };
                this.git.update(cx, |git, cx| {
                    git.refresh(cx);
                    git.notify(notification, cx)
                });
            })
            .ok();
        })
        .detach();
    }

    fn report(&mut self, event: GitEvent, cx: &mut Context<Self>) {
        self.git.update(cx, |git, cx| git.report(event, cx));
    }

    // --- Conflicts ---

    /// The conflicted files under the selected row.
    fn selected_conflicts(&self) -> Vec<Change> {
        self.selected_row()
            .map(|row| {
                row.files
                    .iter()
                    .filter_map(|&index| self.changes.get(index))
                    .filter(|change| change.status == FileStatus::Conflicted)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Merge…: the merge tool for the selected conflicted file.
    fn merge_file(&mut self, _: &MergeFile, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(change) = self.selected_file().cloned()
            && change.status == FileStatus::Conflicted
        {
            open_conflict(&change, window, cx);
        }
    }

    /// Accept Yours / Accept Theirs: the selected conflicted files take one side whole.
    fn accept(&mut self, side: ConflictSide, cx: &mut Context<Self>) {
        let conflicts = self.selected_conflicts();
        let Some(first) = conflicts.first() else {
            return;
        };
        let repo = first.repo;
        let paths: Vec<String> = conflicts
            .iter()
            .filter(|change| change.repo == repo)
            .map(|change| change.relative.clone())
            .collect();
        let count = paths.len();
        let name = file_name(&first.path);
        let git = self.git.clone();
        let task = git.update(cx, |git, cx| git.accept_side(repo, paths, side, cx));
        cx.spawn(async move |_, cx| {
            let result = task.await;
            git.update(cx, |git, cx| match result {
                Ok(()) => {
                    let what = match count {
                        1 => name,
                        _ => trn(count, "{n} file", "{n} files"),
                    };
                    let message = match side {
                        ConflictSide::Ours => trf("Resolved {0}: kept yours", &[&what]),
                        ConflictSide::Theirs => trf("Resolved {0}: took theirs", &[&what]),
                    };
                    git.notify(Notification::success(message), cx)
                }
                Err(err) => git.notify_error(tr("Couldn't resolve the conflict"), &err, cx),
            })
            .ok();
        })
        .detach();
    }

    /// Stash Selected Files…: the Stash Changes dialog for the files under the selected row.
    fn stash_selected(
        &mut self,
        _: &StashSelectedFiles,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let changes: Vec<Change> = self
            .target_changes(cx)
            .into_iter()
            .filter(checkable)
            .collect();
        let Some(first) = changes.first() else {
            return;
        };
        let repo = first.repo;
        let mut paths = Vec::new();
        for change in changes.iter().filter(|change| change.repo == repo) {
            paths.push(change.relative.clone());
            // A rename takes its old path along.
            if let Some(orig) = change.orig_path.as_deref()
                && let Some(relative) = self
                    .git
                    .read(cx)
                    .repos()
                    .get(repo)
                    .and_then(|entry| entry.repo.relative(orig))
            {
                paths.push(relative);
            }
        }
        let untracked = changes
            .iter()
            .any(|change| change.repo == repo && change.status == FileStatus::Untracked);
        window.dispatch_action(
            Box::new(StashSelected {
                repo,
                paths,
                untracked,
            }),
            cx,
        );
    }

    // --- Mouse and menus ---

    fn click_row(
        &mut self,
        key: RowKey,
        click_count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        self.selected = Some(key.clone());
        let row = self.rows.iter().find(|row| row.key == key).cloned();
        match row {
            Some(row) if row.expandable() && click_count == 1 => {
                self.set_expanded(&row.key, !row.expanded, cx)
            }
            Some(_) if click_count == 2 => {
                if let Some(change) = self.selected_file().cloned() {
                    if change.status == FileStatus::Conflicted {
                        open_conflict(&change, window, cx);
                    } else {
                        self.open_diff_of(&change, cx);
                    }
                }
            }
            _ => {}
        }
        cx.notify();
    }

    /// The context menu for a row (or the empty space under the rows) at the cursor.
    fn secondary_click(
        &mut self,
        key: Option<RowKey>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        self.selected = key;
        if let Some(session) = self.selected_claude_session() {
            return self.claude_menu(session, position, window, cx);
        }
        let file = self.selected_file().cloned();
        let row = self.selected_row().cloned();
        let conflicts = self.selected_conflicts();
        if !conflicts.is_empty() && row.as_ref().is_some_and(|row| !self.row_checkable(row)) {
            return self.conflict_menu(file, conflicts, position, window, cx);
        }
        let tracked = self
            .target_changes(cx)
            .iter()
            .any(|change| change.status != FileStatus::Untracked && checkable(change));
        let stashable = self.target_changes(cx).iter().any(checkable);
        let unversioned = row.as_ref().is_some_and(|row| {
            row.files
                .iter()
                .any(|&index| self.changes[index].status == FileStatus::Untracked)
        });
        let ignorable = matches!(
            row.as_ref().map(|row| &row.key),
            Some(RowKey::File(_)) | Some(RowKey::Dir(_, GroupKind::Unversioned, _))
        ) && unversioned;
        let menu = cx.new(|cx| {
            ContextMenu::new(window, cx)
                .entry_if(file.is_some(), tr("Show Diff"), ShowDiff)
                .entry_if(
                    file.as_ref()
                        .is_some_and(|file| file.status != FileStatus::Deleted),
                    tr("Jump to Source"),
                    JumpToSource,
                )
                .separator()
                .entry_if(tracked, tr("Rollback…"), Rollback)
                .entry_if(ignorable, tr("Add to .gitignore"), AddToGitignore)
                .entry_if(unversioned, tr("Delete…"), Delete)
                .separator()
                .entry_if(
                    stashable && row.is_some(),
                    tr("Stash Selected Files…"),
                    StashSelectedFiles,
                )
                .entry(tr("Stash Changes…"), git::StashChanges)
                .separator()
                .entry_if(row.is_some(), tr("Copy Path"), CopyPath)
                .separator()
                .entry(tr("Expand All"), ExpandAll)
                .entry(tr("Collapse All"), CollapseAll)
                .entry(tr("Refresh"), Refresh)
        });
        self.open_menu(menu, position, window, cx);
        cx.notify();
    }

    /// The menu of a Claude changelist: its diffs, the rollback to the text before Claude, back to
    /// "Changes", the session's chat.
    fn claude_menu(
        &mut self,
        session: EntityId,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let file = self.selected_file().cloned();
        let group = matches!(
            self.selected_row().map(|row| &row.key),
            Some(RowKey::Group(..))
        );
        let menu = cx.new(|cx| {
            ContextMenu::new(window, cx)
                .entry_if(file.is_some(), tr("Show Diff"), ShowDiff)
                .entry_if(file.is_some(), tr("Show Diff with HEAD"), ShowDiffWithHead)
                .entry_if(
                    file.as_ref()
                        .is_some_and(|file| file.status != FileStatus::Deleted),
                    tr("Jump to Source"),
                    JumpToSource,
                )
                .separator()
                .entry(
                    if group {
                        tr("Rollback All Claude Changes…")
                    } else {
                        tr("Rollback to Before Claude…")
                    },
                    Rollback,
                )
                .entry(tr("Move to Changes"), MoveToChanges)
                .separator()
                .entry(tr("Show Session"), claude::ShowSession(session))
                .separator()
                .entry(tr("Stash Selected Files…"), StashSelectedFiles)
                .entry(tr("Copy Path"), CopyPath)
                .separator()
                .entry(tr("Expand All"), ExpandAll)
                .entry(tr("Collapse All"), CollapseAll)
                .entry(tr("Refresh"), Refresh)
        });
        self.open_menu(menu, position, window, cx);
        cx.notify();
    }

    /// The menu of conflicted files: Merge…, Accept Yours, Accept Theirs.
    fn conflict_menu(
        &mut self,
        file: Option<Change>,
        conflicts: Vec<Change>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mergeable = file
            .as_ref()
            .is_some_and(|file| file.conflict.is_none_or(ConflictKind::mergeable));
        let on_disk = file.as_ref().is_some_and(|file| file.path.exists());
        let several = conflicts.len() > 1;
        let menu = cx.new(|cx| {
            ContextMenu::new(window, cx)
                .entry_if(mergeable, tr("Merge…"), MergeFile)
                .entry(
                    if several {
                        tr("Accept Yours for All")
                    } else {
                        tr("Accept Yours")
                    },
                    AcceptYours,
                )
                .entry(
                    if several {
                        tr("Accept Theirs for All")
                    } else {
                        tr("Accept Theirs")
                    },
                    AcceptTheirs,
                )
                .separator()
                .entry(tr("Resolve Conflicts…"), git::ResolveConflicts)
                .separator()
                .entry_if(on_disk, tr("Jump to Source"), JumpToSource)
                .entry(tr("Copy Path"), CopyPath)
        });
        self.open_menu(menu, position, window, cx);
        cx.notify();
    }

    fn open_menu(
        &mut self,
        menu: Entity<ContextMenu>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
    }

    /// Closes the menu (if it is still the same one); Esc in it returns focus to the list.
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

    /// ⇧F10: the menu for the selected row, below it.
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
        let (index, depth) = self
            .selected_index()
            .map(|index| (index as f32 + 1., self.rows[index].depth))
            .unwrap_or((0., 0));
        let position = gpui::point(
            bounds.left() + px(ROW_INSET + ROW_PADDING + depth as f32 * INDENT + CHEVRON_WIDTH),
            bounds.top() + offset.y + px(index * ROW_HEIGHT),
        );
        self.secondary_click(self.selected.clone(), position, window, cx);
    }

    /// The history of recent messages, at the button.
    fn show_history(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(repo) = self.message_repo(cx) else {
            return;
        };
        let read = cx.background_spawn(async move {
            flux_git::recent_messages(&repo, HISTORY_LIMIT).unwrap_or_default()
        });
        cx.spawn_in(window, async move |this, cx| {
            let messages = read.await;
            this.update_in(cx, |this, window, cx| {
                if messages.is_empty() {
                    let message = tr("No commit messages yet");
                    return this.report(GitEvent::Message(message.into()), cx);
                }
                // Items dispatch to the message field: focus it first.
                window.focus(&this.message.focus_handle(cx));
                let menu = cx.new(|cx| {
                    messages.iter().enumerate().fold(
                        ContextMenu::new(window, cx),
                        |menu, (index, message)| {
                            menu.entry(history_label(message), UseMessage(index))
                        },
                    )
                });
                this.history = messages;
                this.open_menu(menu, position, window, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // --- The message ---

    fn message_text(&self, cx: &App) -> String {
        self.message.read(cx).document.text().to_string()
    }

    /// Puts a message into the field (Amend, the history, a merge's message): one undoable edit,
    /// and the field shows its start (a long first line doesn't leave it scrolled to its end).
    pub(crate) fn set_message(&mut self, text: &str, cx: &mut Context<Self>) {
        self.message.update(cx, |editor, cx| {
            let new = Rope::from_str(text);
            if let Some((range, text)) = difference(editor.document.text(), &new) {
                editor.replace_ranges(vec![(range, text)], cx);
                editor
                    .document
                    .set_selection(flux_core::Selection::point(0));
                editor.scroll = gpui::point(0., 0.);
                editor.autoscroll = None;
                cx.notify();
            }
        });
    }

    /// The repository whose history and last commit the message field uses: of the first checked
    /// change, otherwise the first repository.
    fn message_repo(&self, cx: &App) -> Option<flux_git::Repo> {
        let git = self.git.read(cx);
        let repo = self
            .changes
            .iter()
            .find(|change| git.check_state(change) != CheckState::Unchecked)
            .map(|change| change.repo)
            .unwrap_or(0);
        git.repos().get(repo).map(|entry| entry.repo.clone())
    }

    /// Amend on: the last commit's message comes into an empty field. Off: it goes away again,
    /// unless it was edited. Not while a merge is in progress: its commit concludes the merge.
    fn toggle_amend(&mut self, cx: &mut Context<Self>) {
        if !self.amend && !self.merging_repos(cx).is_empty() {
            let message = tr("A merge is in progress: commit it instead of amending");
            return self.report(GitEvent::Message(message.into()), cx);
        }
        self.amend = !self.amend;
        cx.notify();
        if !self.amend {
            if self.amend_message.take().as_deref() == Some(self.message_text(cx).as_str()) {
                self.set_message("", cx);
            }
            return;
        }
        if !self.message_text(cx).trim().is_empty() {
            return;
        }
        let Some(repo) = self.message_repo(cx) else {
            return;
        };
        let read = cx.background_spawn(async move { flux_git::head_message(&repo) });
        cx.spawn(async move |this, cx| {
            let Ok(Some(message)) = read.await else {
                return;
            };
            this.update(cx, |this, cx| {
                if this.amend && this.message_text(cx).trim().is_empty() {
                    this.set_message(&message, cx);
                    this.amend_message = Some(message);
                }
            })
            .ok();
        })
        .detach();
    }

    // --- The message Claude writes ---

    /// Whether Claude can write the message: Claude Code is on and `claude` is ready.
    fn can_generate(&self, cx: &App) -> bool {
        claude::enabled(cx) && self.claude.read(cx).is_ready()
    }

    /// "Generate Commit Message with Claude" (as JetBrains AI Assistant's): the checked changes as
    /// they would be committed (partly checked files with their checked hunks, open documents with
    /// their unsaved text) and the repository's recent subjects go to `claude -p` (Haiku); the
    /// answer replaces the message, one undoable edit. A second click stops it.
    fn generate_message(&mut self, cx: &mut Context<Self>) {
        if self.generating.take().is_some() {
            return cx.notify();
        }
        let cli = self
            .claude
            .read(cx)
            .cli()
            .cli()
            .cloned()
            .filter(|_| self.can_generate(cx));
        let Some(cli) = cli else {
            let message = tr("Claude Code isn't ready");
            return self.report(GitEvent::Message(message.into()), cx);
        };
        let items: Vec<(Change, bool)> = {
            let git = self.git.read(cx);
            self.changes
                .iter()
                .filter(|change| checkable(change))
                .map(|change| (change, git.check_state(change)))
                .filter(|(_, state)| *state != CheckState::Unchecked)
                .map(|(change, state)| (change.clone(), state == CheckState::Partial))
                .collect()
        };
        if items.is_empty() {
            return self.report(GitEvent::Message(tr("No changes are checked").into()), cx);
        }
        let Some(repo) = self.message_repo(cx) else {
            return;
        };
        // The commit saves open documents first: the message follows their unsaved text.
        let open: HashMap<PathBuf, String> = self
            .git
            .read(cx)
            .editors()
            .iter()
            .filter_map(|editor| {
                let document = &editor.read(cx).document;
                let path = document.path().filter(|_| document.is_modified())?;
                Some((path.to_path_buf(), document.text().to_string()))
            })
            .collect();
        let bases: Vec<Task<Option<Arc<str>>>> = items
            .iter()
            .map(|(change, _)| {
                if change.status == FileStatus::Untracked {
                    Task::ready(None)
                } else {
                    self.git
                        .update(cx, |git, cx| git.base_text(&change.path, cx))
                }
            })
            .collect();
        self.generating = Some(cx.spawn(async move |this, cx| {
            let mut files = Vec::new();
            for ((change, partial), base) in items.into_iter().zip(bases) {
                let base = base.await.map(|base| base.to_string()).unwrap_or_default();
                let current = match open.get(&change.path) {
                    Some(text) => text.clone(),
                    None => {
                        let path = change.path.clone();
                        cx.background_spawn(async move {
                            std::fs::read_to_string(&path).unwrap_or_default()
                        })
                        .await
                    }
                };
                let new = if partial {
                    let path = change.path.clone();
                    this.update(cx, |this, cx| {
                        let git = this.git.read(cx);
                        partial_content(&base, &current, |hunk| git.is_hunk_included(&path, hunk))
                    })
                    .unwrap_or(current)
                } else {
                    current
                };
                files.push(MessageFile {
                    relative: change.relative,
                    status: change.status,
                    old: base,
                    new,
                });
            }
            let answer = cx
                .background_spawn(async move {
                    let subjects =
                        flux_git::recent_messages(&repo, MESSAGE_EXAMPLES).unwrap_or_default();
                    let prompt = message_prompt(&files, &subjects);
                    cli.ask(&repo.work_dir, &prompt, Some(MESSAGE_MODEL))
                })
                .await;
            this.update(cx, |this, cx| {
                this.generating = None;
                let failure = match answer.map(|text| clean_message(&text)) {
                    Ok(message) if !message.is_empty() => {
                        this.set_message(&message, cx);
                        None
                    }
                    Ok(_) => Some(tr("Claude's answer was empty").to_string()),
                    Err(err) => Some(err),
                };
                if let Some(failure) = failure {
                    let notification =
                        Notification::error(tr("Claude couldn't write the commit message"))
                            .body(failure);
                    this.git.update(cx, |git, cx| git.notify(notification, cx));
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    // --- Commit ---

    /// Commits the checked changes of every repository with the message; with `push`, the push
    /// dialog opens after. Open documents of the committed files are saved first.
    fn commit(&mut self, push: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.committing {
            return;
        }
        let message = self.message_text(cx);
        if message.trim().is_empty() {
            self.report(GitEvent::Message(tr("Enter a commit message").into()), cx);
            return self.focus_message(window, cx);
        }
        let merging = self.merging_repos(cx);
        let problem = {
            let git = self.git.read(cx);
            let states: Vec<CheckState> = self
                .changes
                .iter()
                .map(|change| git.check_state(change))
                .collect();
            merge_commit_problem(&self.changes, &states, &merging)
        };
        if let Some(problem) = problem {
            return self.report(GitEvent::Message(problem.into()), cx);
        }
        let (included, editors) = {
            let git = self.git.read(cx);
            let included: Vec<Included> = self
                .changes
                .iter()
                .filter(|change| checkable(change))
                .map(|change| (change, git.check_state(change)))
                .filter(|(_, state)| *state != CheckState::Unchecked)
                .map(|(change, state)| Included {
                    change: change.clone(),
                    partial: state == CheckState::Partial,
                    // A rename commits the removal of the old path too.
                    orig_relative: change
                        .orig_path
                        .as_deref()
                        .and_then(|orig| git.repos().get(change.repo)?.repo.relative(orig)),
                })
                .collect();
            (included, git.editors())
        };
        // A merge whose result is HEAD's tree still needs its commit.
        if included.is_empty() && merging.is_empty() {
            return self.report(GitEvent::Message(tr("No changes are checked").into()), cx);
        }
        let paths: HashSet<PathBuf> = included
            .iter()
            .map(|item| item.change.path.clone())
            .collect();
        let to_save: Vec<Entity<Editor>> = editors
            .into_iter()
            .filter(|editor| {
                let document = &editor.read(cx).document;
                document.is_modified() && document.path().is_some_and(|path| paths.contains(path))
            })
            .collect();
        let saves: Vec<Task<bool>> = to_save
            .into_iter()
            .map(|editor| editor.update(cx, |editor, cx| editor.save(cx)))
            .collect();
        let bases: Vec<(PathBuf, Task<Option<std::sync::Arc<str>>>)> = included
            .iter()
            .filter(|item| item.partial)
            .map(|item| {
                let path = item.change.path.clone();
                let base = self.git.update(cx, |git, cx| git.base_text(&path, cx));
                (path, base)
            })
            .collect();
        // Wholly committed files leave their Claude changelist: Claude's next edit starts anew.
        let claude_committed: Vec<(EntityId, PathBuf)> = included
            .iter()
            .filter(|item| !item.partial)
            .filter_map(|item| self.claude_file(&item.change))
            .collect();
        self.committing = true;
        cx.notify();
        let amend = self.amend && merging.is_empty();
        cx.spawn_in(window, async move |this, cx| {
            let outcome =
                run_commit(&this, included, merging, saves, bases, message, amend, cx).await;
            this.update(cx, |this, cx| {
                this.committing = false;
                match outcome {
                    Ok((count, summary)) => {
                        for (session, path) in claude_committed {
                            this.forget_claude_files(session, &[path], cx);
                        }
                        this.set_message("", cx);
                        this.amend = false;
                        this.amend_message = None;
                        this.merge_message = None;
                        let files = trn(count, "{n} file", "{n} files");
                        let title = trf("Committed {0}", &[&files]);
                        let notification = Notification::success(title).body(summary);
                        this.git.update(cx, |git, cx| git.notify(notification, cx));
                        if push {
                            cx.emit(CommitPanelEvent::Push);
                        }
                    }
                    // A commit that didn't happen is an outcome, not a hint: a card.
                    Err(GitEvent::Message(message)) => {
                        let notification = Notification::warning(message);
                        this.git.update(cx, |git, cx| git.notify(notification, cx))
                    }
                    Err(event) => this.report(event, cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // --- Rendering ---

    /// The tabs: "Commit" and "Stash", as the tool window tabs of JetBrains IDEs.
    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let tab =
            |id: &'static str, label: &'static str, tab: CommitTab, cx: &mut Context<Self>| {
                let active = self.tab == tab;
                let group = SharedString::from(format!("{id}-group"));
                div()
                    .id(id)
                    .group(group.clone())
                    .relative()
                    .h_full()
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .child(
                        ui::section_label(label, ui)
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
                                .bottom(px(7.))
                                .h(px(2.))
                                .rounded(px(1.))
                                .bg(ui.accent),
                        )
                    })
                    // A click on a tab is a move into it: its list takes focus.
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.set_tab(tab, cx);
                        this.focus_changes(window, cx);
                    }))
            };
        div()
            .flex_none()
            .h(px(HEADER_HEIGHT))
            .pl(px(ROW_INSET + ROW_PADDING + 2.))
            .pr(px(ROW_INSET))
            .flex()
            .items_center()
            .gap_4()
            .child(tab(
                "commit-tab-commit",
                tr("Commit"),
                CommitTab::Commit,
                cx,
            ))
            .child(tab("commit-tab-stash", tr("Stash"), CommitTab::Stash, cx))
    }

    /// The toolbar of the tab shown, under the tabs.
    fn render_toolbar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let bar = div()
            .flex_none()
            .h(px(TOOLBAR_HEIGHT))
            .pl(px(ROW_INSET + 2.))
            .pr(px(ROW_INSET))
            .flex()
            .items_center();
        if self.tab == CommitTab::Stash {
            return bar.child(self.stash.read(cx).toolbar(window, cx));
        }
        let ui = Theme::ui(cx);
        let button = |id: &'static str,
                      name: IconName,
                      label: &'static str,
                      action: Box<dyn Action>,
                      on: Option<bool>| {
            let keys = ui::shortcut_in(action.as_ref(), &self.focus_handle, window);
            let button = match on {
                Some(on) => ui::toggle_button(id, name, on, ui),
                None => ui::icon_button(id, name, ui),
            };
            let focus = self.focus_handle.clone();
            button
                .tooltip(ui::tooltip(label, keys))
                .on_click(move |_, window, cx| {
                    window.focus(&focus);
                    window.dispatch_action(action.boxed_clone(), cx)
                })
        };
        bar.gap_0p5()
            .child(button(
                "commit-refresh",
                IconName::Refresh,
                tr("Refresh"),
                Box::new(Refresh),
                None,
            ))
            .child(button(
                "commit-rollback",
                IconName::Rollback,
                tr("Rollback…"),
                Box::new(Rollback),
                None,
            ))
            .child(button(
                "commit-diff",
                IconName::Diff,
                tr("Show Diff"),
                Box::new(ShowDiff),
                None,
            ))
            .child(div().w_1())
            .child(button(
                "commit-group",
                IconName::Folder,
                tr("Group by Directory"),
                Box::new(ToggleGroupByDirectory),
                Some(self.group_by_directory),
            ))
            .child(button(
                "commit-expand",
                IconName::ExpandAll,
                tr("Expand All"),
                Box::new(ExpandAll),
                None,
            ))
            .child(button(
                "commit-collapse",
                IconName::CollapseAll,
                tr("Collapse All"),
                Box::new(CollapseAll),
                None,
            ))
    }

    /// A banner per repository in the middle of an operation: what is going on (merging,
    /// rebasing 2/5…), its conflicts, and Continue / Skip / Abort.
    fn render_banners(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let ui = Theme::ui(cx);
        let git = self.git.read(cx);
        let several = git.repos().len() > 1;
        let mut banners = Vec::new();
        for repo in git.repos_in_progress() {
            let operation = git.operation(repo);
            let refs = git.refs(repo);
            let current = git
                .repos()
                .get(repo)
                .and_then(|entry| entry.status.branch.label())
                .unwrap_or_else(|| tr("HEAD").to_string());
            let Some((text, actions)) = banner_text(&operation, &current, |oid| {
                refs.name_of(oid).map(str::to_string)
            }) else {
                continue;
            };
            let text = if several {
                format!("{} · {text}", git.repo_name(repo))
            } else {
                text
            };
            let conflicts = self
                .changes
                .iter()
                .filter(|change| change.repo == repo && change.status == FileStatus::Conflicted)
                .count();
            let color = if conflicts > 0 {
                ui.vcs_conflict
            } else {
                ui.warning
            };
            let detail: AnyElement = if conflicts > 0 {
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(div().text_color(ui.vcs_conflict).child(trn(
                        conflicts,
                        "{n} conflict",
                        "{n} conflicts",
                    )))
                    .child(
                        link(("banner-resolve", repo), tr("Resolve…"), ui.accent_text, ui)
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(git::ResolveConflicts), cx)
                            }),
                    )
                    .into_any_element()
            } else if operation.state == RepoState::Merging {
                div()
                    .text_color(ui.text_muted)
                    .child(tr("Commit to finish the merge"))
                    .into_any_element()
            } else {
                div().into_any_element()
            };
            let buttons = actions.into_iter().map(|action| {
                let (label, id, color, enabled, dispatch): (&str, usize, _, bool, Box<dyn Action>) =
                    match action {
                        BannerAction::Continue => (
                            tr("Continue"),
                            0,
                            ui.accent_text,
                            conflicts == 0,
                            Box::new(git::ContinueRepoOperation { repo }),
                        ),
                        BannerAction::Skip => (
                            tr("Skip"),
                            1,
                            ui.text_muted,
                            true,
                            Box::new(git::SkipRepoCommit { repo }),
                        ),
                        BannerAction::Abort => (
                            tr("Abort"),
                            2,
                            ui.text_muted,
                            true,
                            Box::new(git::AbortRepoOperation { repo }),
                        ),
                    };
                link(("banner-action", repo * 4 + id), label, color, ui)
                    .when(!enabled, |link| {
                        link.opacity(0.45)
                            .tooltip(ui::tooltip(tr("Resolve the conflicts first"), None))
                    })
                    .when(enabled, |link| {
                        link.on_click(move |_, window, cx| {
                            window.dispatch_action(dispatch.boxed_clone(), cx)
                        })
                    })
            });
            banners.push(
                div()
                    .flex_none()
                    .mx(px(ROW_INSET))
                    .mb_1p5()
                    .px_2()
                    .py_1p5()
                    .flex()
                    .gap_2()
                    .rounded(px(RADIUS_MD))
                    .bg(UiColors::tint(color, 0.10))
                    .border_1()
                    .border_color(UiColors::tint(color, 0.22))
                    .text_size(px(theme::TEXT_SM))
                    .child(
                        div()
                            .flex_none()
                            .pt(px(1.))
                            .child(icon(IconName::Merge, color).size(px(14.))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_color(ui.foreground)
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(text),
                            )
                            .child(detail)
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .gap_1()
                                    .ml(px(-4.))
                                    .children(buttons),
                            ),
                    )
                    .into_any_element(),
            );
        }
        banners
    }

    fn render_row(&self, index: usize, focused: bool, cx: &mut Context<Self>) -> AnyElement {
        let ui = Theme::ui(cx);
        let Some(row) = self.rows.get(index) else {
            return div().into_any_element();
        };
        let selected = self.selected.as_ref() == Some(&row.key);
        let state = self.row_state(row, cx);
        let checkable = self.row_checkable(row);
        let (icon_element, name, name_color, detail, strike) = match &row.kind {
            RowKind::Repo { name, branch } => (
                icon(IconName::Branch, ui.violet)
                    .size(px(14.))
                    .into_any_element(),
                name.clone(),
                ui.foreground,
                branch.clone(),
                false,
            ),
            // The count is a bare number: "Unversioned Files 2 files" doesn't fit the island in
            // Russian («Неотслеживаемые файлы 2 файла»).
            // A Claude changelist: Claude's mark and the session's title.
            RowKind::Group(GroupKind::Claude(session)) => (
                icon(IconName::Claude, ui.accent_text)
                    .size(px(13.))
                    .into_any_element(),
                format!(
                    "Claude · {}",
                    self.claude_groups.title(*session).unwrap_or_default()
                ),
                ui.foreground,
                Some(row.files.len().to_string()),
                false,
            ),
            RowKind::Group(group) => (
                div().into_any_element(),
                group.label().to_string(),
                if *group == GroupKind::Conflicts {
                    ui.vcs_conflict
                } else {
                    ui.foreground
                },
                Some(row.files.len().to_string()),
                false,
            ),
            RowKind::Dir { label } => (
                folder_icon(row.expanded, &ui).render().into_any_element(),
                label.clone(),
                ui.foreground,
                None,
                false,
            ),
            RowKind::File { name, detail } => {
                let change = &self.changes[row.files[0]];
                (
                    file_icon(name, &ui).render().into_any_element(),
                    name.clone(),
                    git::status_color(change.status, &ui),
                    detail.clone(),
                    change.status == FileStatus::Deleted,
                )
            }
        };
        let group_row = matches!(row.kind, RowKind::Group(_) | RowKind::Repo { .. });
        // The conflicts group offers the Conflicts dialog right on its row.
        let resolve = matches!(row.kind, RowKind::Group(GroupKind::Conflicts)).then(|| {
            link(("commit-resolve", index), tr("Resolve"), ui.accent_text, ui)
                .flex_none()
                .on_click(|_, window, cx| {
                    cx.stop_propagation();
                    window.dispatch_action(Box::new(git::ResolveConflicts), cx)
                })
        });
        let key = row.key.clone();
        let (click, secondary, toggle) = (key.clone(), key.clone(), key.clone());
        let body = div()
            .size_full()
            .flex()
            .items_center()
            .gap_1p5()
            .pl(px(ROW_PADDING + row.depth as f32 * INDENT))
            .pr_2()
            .rounded(px(RADIUS_SM))
            .map(|body| match (selected, focused) {
                (true, true) => body.bg(ui.list_selected),
                (true, false) => body.bg(ui.list_selected_inactive),
                _ => body.group_hover(ROW_GROUP, move |style| style.bg(ui.hover)),
            })
            .child(chevron(row.expandable(), row.expanded, ui))
            .child(if checkable {
                ui::checkbox(("check", index), state, ui)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        window.focus(&this.focus_handle);
                        this.toggle_row(&toggle, cx);
                        cx.stop_propagation();
                    }))
                    .into_any_element()
            } else {
                // Conflicted files can't be checked: an empty column keeps the names aligned.
                div().flex_none().w(px(14.)).into_any_element()
            })
            .child(icon_element)
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(name_color)
                    .when(group_row, |name| name.font_weight(FontWeight::MEDIUM))
                    .when(strike, |name| name.line_through())
                    .child(name),
            )
            .children(detail.map(|detail| {
                div()
                    .flex_none()
                    .min_w_0()
                    .truncate()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.dim)
                    .child(detail)
            }))
            .children(resolve.map(|resolve| div().flex_1().flex().justify_end().child(resolve)));
        div()
            .id(index)
            .group(ROW_GROUP)
            .h(px(ROW_HEIGHT))
            .w_full()
            .px(px(ROW_INSET))
            .whitespace_nowrap()
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
            .child(body)
            .into_any_element()
    }

    /// No changes, no repository, or still looking for repositories.
    fn render_empty(&self, cx: &App) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let git = self.git.read(cx);
        let (glyph, title, hint) = if git.is_discovering() {
            (IconName::Refresh, tr("Looking for repositories…"), None)
        } else if git.repos().is_empty() {
            (
                IconName::Branch,
                tr("No Git repository"),
                Some(tr("Open a folder with a repository: ⌘O")),
            )
        } else {
            (
                IconName::Check,
                tr("No changes"),
                Some(tr("Edits to the project's files show up here")),
            )
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .px_4()
            .child(icon(glyph, ui.dim).size(px(20.)))
            .child(div().text_color(ui.text_muted).child(title))
            .children(hint.map(|hint| {
                div()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.dim)
                    .text_center()
                    .child(hint)
            }))
    }

    fn render_message(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let focused = self.message.focus_handle(cx).is_focused(window);
        let generate = self.can_generate(cx);
        // The text keeps clear of the buttons in the corner.
        let buttons = if generate { 2. } else { 1. };
        div()
            .flex_none()
            .relative()
            .h(px(MESSAGE_HEIGHT))
            .mx_2()
            .pl_1()
            .pt_1()
            .pr(px(buttons * (ui::ICON_BUTTON_SIZE + 2.) + 4.))
            .rounded(px(RADIUS_MD))
            .bg(ui.input_background)
            .border_1()
            .border_color(if focused {
                ui.focus_border
            } else {
                ui.input_border
            })
            .when(focused, |field| field.shadow(ui::focus_ring(ui)))
            .child(self.message.clone())
            .child(
                div()
                    .absolute()
                    .top(px(3.))
                    .right(px(3.))
                    .flex()
                    .items_center()
                    .gap_0p5()
                    .children(generate.then(|| self.render_generate_button(cx)))
                    .child(
                        ui::icon_button("commit-history", IconName::History, ui)
                            .tooltip(ui::tooltip(tr("Commit Message History"), None))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.show_history(event.position, window, cx)
                                }),
                            ),
                    ),
            )
    }

    /// Claude's mark in the message field: writes the message; while it does, the mark pulses and
    /// a click stops it.
    fn render_generate_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let ui = Theme::ui(cx);
        let generating = self.generating.is_some();
        let button = if generating {
            ui::toggle_button("commit-generate", IconName::Claude, true, ui)
                .tooltip(ui::tooltip(tr("Stop Generating"), None))
        } else {
            ui::icon_button("commit-generate", IconName::Claude, ui).tooltip(ui::tooltip(
                tr("Generate Commit Message with Claude"),
                None,
            ))
        }
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
            cx.stop_propagation();
            this.generate_message(cx)
        }));
        if !generating {
            return button.into_any_element();
        }
        div()
            .child(button)
            .with_animation(
                "commit-generating",
                Animation::new(Duration::from_millis(1000)).repeat(),
                |button, delta| button.opacity(0.45 + 0.55 * (1. - (2. * delta - 1.).abs())),
            )
            .into_any_element()
    }

    fn render_footer(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let merging = !self.merging_repos(cx).is_empty();
        // A merge whose result is HEAD's tree has no changes, and still needs its commit.
        let can_commit = !self.committing && (!self.changes.is_empty() || merging);
        let commit_keys = ui::shortcut_for(&CommitChanges, window);
        let push_keys = ui::shortcut_for(&CommitAndPush, window);
        div()
            .flex_none()
            .px_2()
            .pt_2()
            .pb_2()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id("commit-amend")
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .cursor_pointer()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.text_muted)
                            .hover(move |style| style.text_color(ui.foreground))
                            .when(merging, |amend| {
                                amend.opacity(0.5).tooltip(ui::tooltip(
                                    tr("A merge is in progress: commit it instead of amending"),
                                    None,
                                ))
                            })
                            .on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_amend(cx)),
                            )
                            .child(ui::checkbox(
                                "commit-amend-check",
                                if self.amend {
                                    CheckState::Checked
                                } else {
                                    CheckState::Unchecked
                                },
                                ui,
                            ))
                            .child(tr("Amend")),
                    )
                    .child(div().flex_1())
                    .children(self.committing.then(|| {
                        div()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.dim)
                            .child(tr("Committing…"))
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        ui::primary_button("commit-button", tr("Commit"), can_commit, ui)
                            .tooltip(ui::tooltip(tr("Commit"), commit_keys))
                            .when(can_commit, |button| {
                                button.on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.commit(false, window, cx)
                                }))
                            }),
                    )
                    .child(
                        ui::text_button("commit-push-button", tr("Commit and Push…"), false, ui)
                            .tooltip(ui::tooltip(tr("Commit and Push…"), push_keys))
                            .when(!can_commit, |button| button.opacity(0.5))
                            .when(can_commit, |button| {
                                button.on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.commit(true, window, cx)
                                }))
                            }),
                    ),
            )
    }
}

/// Why the checked changes can't be committed while a merge is in progress: its commit concludes
/// it, so it takes every change of its repository whole (`states` — the checkbox of each change),
/// and no file may be conflicted. `None` — they can.
pub(crate) fn merge_commit_problem(
    changes: &[Change],
    states: &[CheckState],
    merging: &[usize],
) -> Option<&'static str> {
    let in_merge = |change: &&Change| merging.contains(&change.repo);
    if changes
        .iter()
        .filter(in_merge)
        .any(|change| change.status == FileStatus::Conflicted)
    {
        return Some(tr("Resolve the conflicts first"));
    }
    let partly = changes.iter().zip(states).any(|(change, state)| {
        merging.contains(&change.repo)
            && change.status != FileStatus::Untracked
            && *state != CheckState::Checked
    });
    partly.then(|| tr("A merge is committed as a whole: check all its changes"))
}

/// A conflicted file: the merge tool when both sides have a text, otherwise the Conflicts dialog
/// (a file one side deleted is resolved by taking a side).
fn open_conflict(change: &Change, window: &mut Window, cx: &mut App) {
    let action: Box<dyn Action> = if change.conflict.is_none_or(ConflictKind::mergeable) {
        Box::new(git::OpenMerge {
            repo: change.repo,
            path: change.path.clone(),
        })
    } else {
        Box::new(git::ResolveConflicts)
    };
    window.dispatch_action(action, cx);
}

/// What the banner of an operation in progress offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BannerAction {
    Continue,
    Skip,
    Abort,
}

/// The banner of an operation in progress: what is going on ("Rebasing main onto origin/main ·
/// 2/5") and what can be done. `current` — the current branch (or short hash); `name_of` names a
/// commit by a branch or tag pointing at it. `None` — nothing in progress.
pub(crate) fn banner_text(
    operation: &Operation,
    current: &str,
    name_of: impl Fn(&str) -> Option<String>,
) -> Option<(String, Vec<BannerAction>)> {
    let short = |oid: &str| oid.chars().take(7).collect::<String>();
    let commit = |oid: &Option<String>| -> String {
        oid.as_deref()
            .map(|oid| name_of(oid).unwrap_or_else(|| short(oid)))
            .unwrap_or_default()
    };
    use BannerAction::*;
    Some(match operation.state {
        RepoState::Merging => {
            let incoming = operation
                .incoming_name
                .clone()
                .unwrap_or_else(|| commit(&operation.incoming));
            (
                trf("Merging {0} into {1}", &[&incoming, &current]),
                vec![Abort],
            )
        }
        RepoState::Rebasing => {
            let branch = operation
                .rebase_branch
                .clone()
                .unwrap_or_else(|| tr("detached HEAD").to_string());
            let onto = commit(&operation.rebase_onto);
            let mut text = trf("Rebasing {0} onto {1}", &[&branch, &onto]);
            if let Some((step, total)) = operation.step {
                text.push_str(&format!(" · {step}/{total}"));
            }
            (text, vec![Continue, Skip, Abort])
        }
        RepoState::CherryPicking => (
            trf("Cherry-picking {0}", &[&commit(&operation.incoming)]),
            vec![Continue, Abort],
        ),
        RepoState::Reverting => (
            trf("Reverting {0}", &[&commit(&operation.incoming)]),
            vec![Continue, Abort],
        ),
        RepoState::Normal | RepoState::Bisecting => return None,
    })
}

/// A change the commit takes.
struct Included {
    change: Change,
    /// Only some of its hunks.
    partial: bool,
    /// The old path of a rename, relative to the working tree.
    orig_relative: Option<String>,
}

/// Saves the documents, computes the partial contents, and commits each repository. `Ok` — how
/// many files went in and the message's first line.
#[allow(clippy::too_many_arguments)]
async fn run_commit(
    this: &gpui::WeakEntity<CommitPanel>,
    included: Vec<Included>,
    merging: Vec<usize>,
    saves: Vec<Task<bool>>,
    bases: Vec<(PathBuf, Task<Option<std::sync::Arc<str>>>)>,
    message: String,
    amend: bool,
    cx: &mut gpui::AsyncWindowContext,
) -> Result<(usize, String), GitEvent> {
    for save in saves {
        if !save.await {
            return Err(GitEvent::Message(
                tr("Commit canceled: a document couldn't be saved").into(),
            ));
        }
    }
    // The checked changes of partly checked files, applied to their HEAD versions.
    let mut partial: Vec<(PathBuf, String)> = Vec::new();
    for (path, base) in bases {
        let base = base.await.unwrap_or_default();
        let read = path.clone();
        let current = cx
            .background_spawn(async move { std::fs::read_to_string(&read) })
            .await
            .map_err(|err| GitEvent::Message(err.to_string().into()))?;
        let content = this
            .update(cx, |this, cx| {
                let git = this.git.read(cx);
                partial_content(&base, &current, |hunk| git.is_hunk_included(&path, hunk))
            })
            .map_err(|_| GitEvent::Message(tr("Commit canceled").into()))?;
        partial.push((path, content));
    }
    let mut requests: Vec<(usize, CommitRequest)> = Vec::new();
    let count = included.len();
    for item in &included {
        let change = &item.change;
        let mut files = vec![CommitFile {
            path: change.relative.clone(),
            content: match partial.iter().find(|(path, _)| *path == change.path) {
                Some((_, content)) => CommitContent::Partial(content.clone().into_bytes()),
                None => CommitContent::WorkTree,
            },
        }];
        if let Some(orig) = &item.orig_relative {
            files.push(CommitFile {
                path: orig.clone(),
                content: CommitContent::WorkTree,
            });
        }
        match requests.iter_mut().find(|(repo, _)| *repo == change.repo) {
            Some((_, request)) => request.files.extend(files),
            None => requests.push((
                change.repo,
                CommitRequest {
                    message: message.clone(),
                    amend,
                    files,
                    author: None,
                },
            )),
        }
    }
    // A merge in progress is concluded by its commit even when its result is HEAD's tree.
    for repo in merging {
        if !requests.iter().any(|(at, _)| *at == repo) {
            requests.push((
                repo,
                CommitRequest {
                    message: message.clone(),
                    amend: false,
                    files: Vec::new(),
                    author: None,
                },
            ));
        }
    }
    let tasks = this
        .update(cx, |this, cx| {
            requests
                .into_iter()
                .map(|(repo, request)| this.git.update(cx, |git, cx| git.commit(repo, request, cx)))
                .collect::<Vec<_>>()
        })
        .map_err(|_| GitEvent::Message(tr("Commit canceled").into()))?;
    let mut summary = String::new();
    for task in tasks {
        match task.await {
            Ok(result) => summary = result.summary,
            Err(err) => {
                let title = tr("Commit failed");
                let notification =
                    crate::git::error_notification(title, &err.to_string(), err.details());
                return Err(GitEvent::Notify(notification));
            }
        }
    }
    Ok((count, summary))
}

/// Asks about rolling back `changes` (untracked files are not rolled back) and does it: git puts
/// the files back, the open documents follow them (one undoable edit each, then saved), and
/// `on_removed` gets the files that are gone from disk.
pub(crate) fn confirm_rollback(
    git: Entity<GitStore>,
    changes: Vec<Change>,
    window: &mut Window,
    cx: &mut App,
    on_removed: impl FnOnce(Vec<PathBuf>, &mut App) + 'static,
) {
    let changes: Vec<Change> = changes
        .into_iter()
        .filter(|change| change.status != FileStatus::Untracked)
        .collect();
    if changes.is_empty() {
        return;
    }
    let added = changes
        .iter()
        .any(|change| matches!(change.status, FileStatus::Added | FileStatus::Renamed));
    let question = match changes.as_slice() {
        [change] => trf("Roll back changes in “{0}”?", &[&file_name(&change.path)]),
        _ => trf(
            "Roll back changes {0}?",
            &[&trn(changes.len(), "in {n} file", "in {n} files")],
        ),
    };
    let mut files = changes
        .iter()
        .take(LISTED_FILES)
        .map(|change| change.relative.clone())
        .collect::<Vec<_>>()
        .join("\n");
    if changes.len() > LISTED_FILES {
        files.push('\n');
        files.push_str(&trf("and {0} more", &[&(changes.len() - LISTED_FILES)]));
    }
    let mut message = tr("The files return to their last committed state.").to_string();
    if added {
        message.push(' ');
        message.push_str(tr(
            "Added files stay on disk as unversioned, unless you delete them too.",
        ));
    }
    let mut dialog = Dialog::warning(question)
        .message(message)
        .details(files)
        .danger(tr("Rollback"));
    if added {
        dialog = dialog.danger(tr("Rollback and Delete Added"));
    }
    let answer = dialog.cancel(tr("Cancel")).show(window, cx);
    cx.spawn(async move |cx: &mut AsyncApp| {
        let delete_added = match answer.await {
            Some(0) => false,
            Some(1) if added => true,
            _ => return,
        };
        let Ok(task) = git.update(cx, |git, cx| {
            git.rollback(changes.clone(), delete_added, cx)
        }) else {
            return;
        };
        let result = task.await;
        cx.update(|cx| {
            if let Err(err) = result {
                return git.update(cx, |git, cx| git.notify_error(tr("Rollback failed"), &err, cx));
            }
            let removed = follow_rollback(&git, &changes, cx);
            let message = match changes.as_slice() {
                [change] => trf("Rolled back: {0}", &[&file_name(&change.path)]),
                _ => trf(
                    "Rolled back {0}",
                    &[&trn(changes.len(), "{n} file", "{n} files")],
                ),
            };
            git.update(cx, |git, cx| git.notify(Notification::success(message), cx));
            if !removed.is_empty() {
                on_removed(removed, cx);
            }
        })
        .ok();
    })
    .detach();
}

/// A file of a Claude changelist an operation acts on.
struct ClaudeTarget {
    change: Change,
    session: EntityId,
    /// As Claude wrote it (the session's key).
    path: PathBuf,
    /// The text before Claude; `None` — Claude created the file.
    original: Option<Arc<str>>,
}

/// Puts the files back as they were before Claude (on a background thread): a text is written
/// atomically, a file Claude created goes to the Trash. The errors, as lines.
fn restore_originals(jobs: &[(PathBuf, Option<Arc<str>>)]) -> Vec<String> {
    let mut errors = Vec::new();
    let mut trash = Vec::new();
    for (path, original) in jobs {
        match original {
            Some(text) => {
                if let Err(err) = write_atomically(path, text) {
                    errors.push(format!("{}: {err}", path.display()));
                }
            }
            None if path.exists() => trash.push(path.clone()),
            None => {}
        }
    }
    if !trash.is_empty()
        && let Err(err) = flux_fs::trash(&trash)
    {
        errors.push(err.to_string());
    }
    errors
}

/// Writes a file through a temporary one and a rename, keeping its permissions (as a document's
/// save): a failure leaves the old file whole.
fn write_atomically(path: &Path, text: &str) -> std::io::Result<()> {
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let permissions = std::fs::metadata(&target).ok().map(|meta| meta.permissions());
    let temp = target.with_file_name(format!(
        ".{}.flux-tmp",
        target.file_name().unwrap_or_default().to_string_lossy()
    ));
    let result = std::fs::write(&temp, text).and_then(|()| {
        if let Some(permissions) = permissions {
            std::fs::set_permissions(&temp, permissions)?;
        }
        std::fs::rename(&temp, &target)
    });
    if result.is_err() {
        std::fs::remove_file(&temp).ok();
    }
    result
}

/// Open documents of rolled-back files take the content from disk (one edit, saved state); the files
/// that are gone are returned.
fn follow_rollback(git: &Entity<GitStore>, changes: &[Change], cx: &mut App) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();
    for change in changes {
        paths.push(change.path.clone());
        paths.extend(change.orig_path.clone());
    }
    let mut removed = Vec::new();
    let editors = git.read(cx).editors();
    for path in &paths {
        let Ok(content) = std::fs::read_to_string(path) else {
            if !path.exists() {
                removed.push(path.clone());
            }
            continue;
        };
        for editor in &editors {
            if editor.read(cx).document.path() != Some(path.as_path()) {
                continue;
            }
            editor.update(cx, |editor, cx| editor.reload(&content, cx));
        }
    }
    removed
}

/// A history item: the message's first line, shortened.
fn history_label(message: &str) -> String {
    let line = message.lines().next().unwrap_or("").trim();
    if line.chars().count() <= HISTORY_ITEM_CHARS {
        return line.to_string();
    }
    let mut short: String = line.chars().take(HISTORY_ITEM_CHARS - 1).collect();
    short.push('…');
    short
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// A file as the commit would take it, for Claude's message.
pub(crate) struct MessageFile {
    pub relative: String,
    pub status: FileStatus,
    /// HEAD's text (empty for a new file).
    pub old: String,
    /// The committed text (empty for a deleted file).
    pub new: String,
}

/// The request for a commit message: the style of the recent subjects, the changed files, their
/// diff — up to [`MESSAGE_DIFF_LIMIT`], then only the names of the rest.
pub(crate) fn message_prompt(files: &[MessageFile], subjects: &[String]) -> String {
    let mut prompt = String::from(
        "Write the commit message for the changes below. Follow the style of the repository's \
         recent commits: the same language, the same format of the subject line, the same level \
         of detail. Reply with the commit message only: no explanations, no quotes, no code \
         fences.\n\n",
    );
    if subjects.is_empty() {
        prompt.push_str("The repository has no commits yet.\n\n");
    } else {
        prompt.push_str("Recent commits:\n");
        for subject in subjects {
            let line = subject.lines().next().unwrap_or_default().trim();
            prompt.push_str(&format!("- {line}\n"));
        }
        prompt.push('\n');
    }
    prompt.push_str("Changed files:\n");
    for file in files {
        prompt.push_str(&format!("{} {}\n", status_letter(file.status), file.relative));
    }
    prompt.push_str("\nDiff:\n");
    let mut size = 0;
    let mut left_out = 0;
    for file in files {
        let diff = unified_diff(&file.relative, &file.old, &file.new);
        if size + diff.len() > MESSAGE_DIFF_LIMIT {
            left_out += 1;
            continue;
        }
        size += diff.len();
        prompt.push_str(&diff);
    }
    if left_out > 0 {
        prompt.push_str(&format!(
            "\n(The diff of {left_out} more file(s) is left out: too long.)\n"
        ));
    }
    prompt
}

/// The letter of `git status --short` for a change.
fn status_letter(status: FileStatus) -> char {
    match status {
        FileStatus::Added | FileStatus::Untracked => 'A',
        FileStatus::Deleted => 'D',
        FileStatus::Renamed => 'R',
        _ => 'M',
    }
}

/// A `git diff`-like text of one file: `--- a/…`, `+++ b/…` and the changed blocks with
/// [`DIFF_CONTEXT`] lines around them (blocks closer than that share their context). Compared
/// without `\r`.
pub(crate) fn unified_diff(relative: &str, old: &str, new: &str) -> String {
    let old = old.replace("\r\n", "\n");
    let new = new.replace("\r\n", "\n");
    let hunks = flux_git::diff_lines(&old, &new);
    if hunks.is_empty() {
        return String::new();
    }
    let old_lines: Vec<&str> = old.split_inclusive('\n').collect();
    let new_lines: Vec<&str> = new.split_inclusive('\n').collect();
    let mut out = format!("--- a/{relative}\n+++ b/{relative}\n");
    let line = |out: &mut String, mark: char, text: &str| {
        out.push(mark);
        out.push_str(text);
        if !text.ends_with('\n') {
            out.push('\n');
        }
    };
    let mut start = 0;
    while start < hunks.len() {
        // The blocks sharing context with the first one.
        let mut end = start + 1;
        while end < hunks.len()
            && hunks[end].old.start.saturating_sub(hunks[end - 1].old.end) <= 2 * DIFF_CONTEXT
        {
            end += 1;
        }
        let (first, last) = (&hunks[start], &hunks[end - 1]);
        let old_from = first.old.start.saturating_sub(DIFF_CONTEXT);
        let old_to = (last.old.end + DIFF_CONTEXT).min(old_lines.len() as u32);
        let new_from = first.new.start - (first.old.start - old_from);
        let new_to = last.new.end + (old_to - last.old.end);
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            old_from + 1,
            old_to - old_from,
            new_from + 1,
            new_to - new_from
        ));
        let mut at = old_from;
        for hunk in &hunks[start..end] {
            for index in at..hunk.old.start {
                line(&mut out, ' ', old_lines[index as usize]);
            }
            for index in hunk.old.clone() {
                line(&mut out, '-', old_lines[index as usize]);
            }
            for index in hunk.new.clone() {
                line(&mut out, '+', new_lines[index as usize]);
            }
            at = hunk.old.end;
        }
        for index in at..old_to {
            line(&mut out, ' ', old_lines[index as usize]);
        }
        start = end;
    }
    out
}

/// Claude's answer as a message: without the code fence or the quotes it may wrap it in.
pub(crate) fn clean_message(answer: &str) -> String {
    let mut text = answer.trim();
    if let Some(rest) = text.strip_prefix("```") {
        // The fence's first line may name a language.
        text = rest.split_once('\n').map_or("", |(_, body)| body);
        text = text.trim_end().strip_suffix("```").unwrap_or(text);
    }
    let text = text.trim();
    let quoted = text.len() >= 2
        && ((text.starts_with('"') && text.ends_with('"'))
            || (text.starts_with('“') && text.ends_with('”')));
    let text = if quoted {
        &text[text.char_indices().nth(1).map_or(0, |(at, _)| at)
            ..text.char_indices().last().map_or(text.len(), |(at, _)| at)]
    } else {
        text
    };
    text.trim().to_string()
}

/// A node's chevron (right when collapsed, down when expanded); for a file, an empty column.
fn chevron(expandable: bool, expanded: bool, ui: UiColors) -> impl IntoElement {
    let glyph = expandable.then_some(if expanded {
        IconName::ChevronDown
    } else {
        IconName::ChevronRight
    });
    div()
        .flex_none()
        .w(px(CHEVRON_WIDTH))
        .h_full()
        .flex()
        .items_center()
        .justify_center()
        .children(glyph.map(|glyph| icon(glyph, ui.dim).size(px(CHEVRON_SIZE))))
}

/// A link-like button: colored text, a backdrop on hover (the banner's actions).
fn link(
    id: impl Into<gpui::ElementId>,
    label: &str,
    color: gpui::Hsla,
    ui: UiColors,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px_1()
        .rounded(px(RADIUS_SM))
        .cursor_pointer()
        .text_color(color)
        .hover(move |style| style.bg(ui.hover))
        .child(label.to_string())
}

/// The island's right edge, in the gap between islands: an accent line on hover and while dragged.
fn resize_handle(resizing: bool, ui: UiColors) -> impl IntoElement {
    div()
        .id("commit-resize")
        .group("commit-resize")
        .absolute()
        .top_0()
        .bottom_0()
        .right(px(-(1. + RESIZE_HANDLE_WIDTH)))
        .w(px(RESIZE_HANDLE_WIDTH))
        .py(px(ui::RADIUS_LG))
        .flex()
        .justify_center()
        .cursor(CursorStyle::ResizeLeftRight)
        .on_drag(DraggedCommitEdge, |_, _, _, cx| {
            cx.new(|_| DraggedCommitEdge)
        })
        .child(
            div()
                .w(px(2.))
                .h_full()
                .rounded(px(1.))
                .bg(ui.focus_border)
                .when(!resizing, |line| {
                    line.invisible()
                        .group_hover("commit-resize", |style| style.visible())
                }),
        )
}

impl Focusable for CommitPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for CommitPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        self.resizing &= cx.has_active_drag();
        let root = div()
            .key_context("CommitPanel")
            .relative()
            .flex_none()
            .w(px(self.width.get()))
            .h_full()
            .flex()
            .flex_col()
            .font_family(theme::UI_FONT)
            .text_size(px(theme::TEXT_MD))
            .text_color(ui.foreground)
            .on_action(cx.listener(|this, _: &CommitChanges, window, cx| {
                if this.tab == CommitTab::Commit {
                    this.commit(false, window, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &CommitAndPush, window, cx| {
                if this.tab == CommitTab::Commit {
                    this.commit(true, window, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleAmend, _, cx| this.toggle_amend(cx)))
            .on_action(cx.listener(|this, _: &GenerateCommitMessage, _, cx| {
                this.generate_message(cx)
            }))
            .on_action(cx.listener(|this, action: &UseMessage, _, cx| {
                if let Some(message) = this.history.get(action.0).cloned() {
                    this.set_message(&message, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ShowCommitTab, window, cx| {
                this.switch_tab(CommitTab::Commit, window, cx);
                this.focus_changes(window, cx);
            }))
            .on_action(
                cx.listener(|this, _: &ShowStashTab, window, cx| this.show_stash(window, cx)),
            )
            // Esc in the message field, with nothing for the field to clear: back to the editor.
            .on_action(
                cx.listener(|_, _: &editor::Cancel, _, cx| cx.emit(CommitPanelEvent::FocusEditor)),
            )
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<DraggedCommitEdge>, _, cx| {
                    let width = f32::from(event.event.position.x - event.bounds.left())
                        - RESIZE_HANDLE_OFFSET;
                    this.width.set(width);
                    this.resizing = true;
                    cx.notify();
                }),
            )
            .child(self.render_tabs(cx))
            .child(self.render_toolbar(window, cx));
        let root = match self.tab {
            CommitTab::Commit => self.render_commit_tab(root, window, cx),
            CommitTab::Stash => root.child(div().flex_1().min_h_0().child(self.stash.clone())),
        };
        root.child(resize_handle(self.resizing, ui)).children(
            self.menu
                .as_ref()
                .map(|menu| ContextMenu::overlay(&menu.menu, menu.position)),
        )
    }
}

impl CommitPanel {
    /// The Commit tab: the banners of operations in progress, the changes, the message, Commit.
    fn render_commit_tab(&mut self, root: Div, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let focused = self.focus_handle.contains_focused(window, cx)
            || self
                .menu
                .as_ref()
                .is_some_and(|menu| menu.menu.focus_handle(cx).is_focused(window));
        let list = if self.rows.is_empty() {
            self.render_empty(cx).into_any_element()
        } else {
            uniform_list(
                "commit-rows",
                self.rows.len(),
                cx.processor(move |this, range: Range<usize>, _window, cx| {
                    range
                        .map(|index| this.render_row(index, focused, cx))
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(self.scroll.clone())
            .size_full()
            .into_any_element()
        };
        let hints = (focused && !self.rows.is_empty()).then(|| {
            div().flex_none().px_3().pb_1p5().child(
                ui::hint_bar(
                    &[("␣", tr("check")), ("↵", tr("diff")), ("F4", tr("open"))],
                    ui,
                )
                .gap_3(),
            )
        });
        let banners = self.render_banners(cx);
        root.children(banners)
            .child(
                div()
                    .key_context("CommitChanges")
                    .track_focus(&self.focus_handle)
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .on_action(cx.listener(|this, _: &SelectNext, _, cx| {
                        this.move_selection(|current, _| current.map_or(0, |i| i + 1), cx)
                    }))
                    .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| {
                        this.move_selection(
                            |current, _| current.map_or(0, |i| i.saturating_sub(1)),
                            cx,
                        )
                    }))
                    .on_action(
                        cx.listener(|this, _: &SelectFirst, _, cx| {
                            this.move_selection(|_, _| 0, cx)
                        }),
                    )
                    .on_action(cx.listener(|this, _: &SelectLast, _, cx| {
                        this.move_selection(|_, len| len - 1, cx)
                    }))
                    .on_action(cx.listener(Self::expand))
                    .on_action(cx.listener(Self::collapse))
                    .on_action(cx.listener(Self::toggle_checked))
                    .on_action(cx.listener(Self::show_diff))
                    .on_action(cx.listener(Self::jump_to_source))
                    .on_action(cx.listener(Self::rollback))
                    .on_action(cx.listener(Self::show_diff_with_head))
                    .on_action(cx.listener(Self::move_to_changes))
                    .on_action(cx.listener(Self::delete))
                    .on_action(cx.listener(Self::copy_path))
                    .on_action(cx.listener(Self::add_to_gitignore))
                    .on_action(cx.listener(Self::merge_file))
                    .on_action(cx.listener(|this, _: &AcceptYours, _, cx| {
                        this.accept(ConflictSide::Ours, cx)
                    }))
                    .on_action(cx.listener(|this, _: &AcceptTheirs, _, cx| {
                        this.accept(ConflictSide::Theirs, cx)
                    }))
                    .on_action(cx.listener(Self::stash_selected))
                    .on_action(
                        cx.listener(|this, _: &ExpandAll, _, cx| this.set_all_expanded(true, cx)),
                    )
                    .on_action(
                        cx.listener(|this, _: &CollapseAll, _, cx| {
                            this.set_all_expanded(false, cx)
                        }),
                    )
                    .on_action(cx.listener(|this, _: &ToggleGroupByDirectory, _, cx| {
                        this.group_by_directory = !this.group_by_directory;
                        this.rebuild(cx);
                    }))
                    .on_action(cx.listener(|this, _: &Refresh, _, cx| {
                        this.git.update(cx, |git, cx| git.refresh(cx))
                    }))
                    .on_action(cx.listener(Self::show_context_menu))
                    .on_action(cx.listener(|this, _: &FocusMessage, window, cx| {
                        this.focus_message(window, cx)
                    }))
                    .on_action(
                        cx.listener(|_, _: &Cancel, _, cx| cx.emit(CommitPanelEvent::FocusEditor)),
                    )
                    .child(
                        div()
                            .id("commit-list")
                            .flex_1()
                            .min_h_0()
                            .pt_0p5()
                            .pb_2()
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
                    .children(hints),
            )
            .child(ui::divider(ui).mx(px(ui::GAP)))
            .child(div().h_2())
            .child(self.render_message(window, cx))
            .child(self.render_footer(window, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(repo: usize, relative: &str, status: FileStatus) -> Change {
        Change {
            repo,
            path: PathBuf::from(format!("/r{repo}/{relative}")),
            relative: relative.into(),
            orig_path: None,
            status,
            conflict: None,
        }
    }

    fn labels(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|row| {
                let text = match &row.kind {
                    RowKind::Repo { name, .. } => format!("repo {name}"),
                    RowKind::Group(GroupKind::Conflicts) => "Conflicts".into(),
                    // The slot of the session's id, without its version.
                    RowKind::Group(GroupKind::Claude(session)) => {
                        format!("Claude {}", session.as_u64() as u32)
                    }
                    RowKind::Group(GroupKind::Changes) => "Changes".into(),
                    RowKind::Group(GroupKind::Unversioned) => "Unversioned".into(),
                    RowKind::Dir { label } => format!("{label}/"),
                    RowKind::File { name, detail } => match detail {
                        Some(detail) => format!("{name} ({detail})"),
                        None => name.clone(),
                    },
                };
                format!("{}{text}", "  ".repeat(row.depth))
            })
            .collect()
    }

    fn one_repo() -> Vec<RepoLabel> {
        vec![RepoLabel {
            name: "demo".into(),
            branch: Some("main".into()),
        }]
    }

    #[test]
    fn changes_group_by_directory_with_joined_chains() {
        let changes = vec![
            change(0, "src/app/ui/view.rs", FileStatus::Modified),
            change(0, "src/main.rs", FileStatus::Modified),
            change(0, "README.md", FileStatus::Deleted),
            change(0, "file10.rs", FileStatus::Untracked),
            change(0, "file2.rs", FileStatus::Untracked),
        ];
        let rows = build_rows(&changes, &one_repo(), true, &HashSet::new(), &ClaudeGroups::default());
        assert_eq!(
            labels(&rows),
            vec![
                "Changes",
                "  src/",
                "    app/ui/",
                "      view.rs",
                "    main.rs",
                "  README.md",
                "Unversioned",
                "  file2.rs",
                "  file10.rs",
            ]
        );
        // A node knows every file under it.
        assert_eq!(rows[1].files.len(), 2);
        assert_eq!(rows[0].files.len(), 3);
    }

    #[test]
    fn the_flat_view_shows_directories_after_names() {
        let changes = vec![
            change(0, "src/main.rs", FileStatus::Modified),
            change(0, "Cargo.toml", FileStatus::Modified),
        ];
        let rows = build_rows(&changes, &one_repo(), false, &HashSet::new(), &ClaudeGroups::default());
        assert_eq!(
            labels(&rows),
            vec!["Changes", "  Cargo.toml", "  main.rs (src)"]
        );
    }

    #[test]
    fn collapsed_nodes_hide_their_contents_and_several_repositories_get_nodes() {
        let changes = vec![
            change(0, "a.rs", FileStatus::Modified),
            change(1, "lib/b.rs", FileStatus::Added),
        ];
        let repos = vec![
            RepoLabel {
                name: "app".into(),
                branch: Some("main".into()),
            },
            RepoLabel {
                name: "core".into(),
                branch: None,
            },
        ];
        let collapsed: HashSet<RowKey> = [RowKey::Dir(1, GroupKind::Changes, "lib".into())].into();
        let rows = build_rows(&changes, &repos, true, &collapsed, &ClaudeGroups::default());
        assert_eq!(
            labels(&rows),
            vec![
                "repo app",
                "  Changes",
                "    a.rs",
                "repo core",
                "  Changes",
                "    lib/"
            ]
        );
        assert!(!rows[5].expanded);
        let collapsed: HashSet<RowKey> = [RowKey::Repo(0)].into();
        let rows = build_rows(&changes, &repos, true, &collapsed, &ClaudeGroups::default());
        assert_eq!(labels(&rows)[0..2], ["repo app", "repo core"]);
    }

    #[test]
    fn conflicted_files_come_first_in_their_own_group() {
        let changes = vec![
            change(0, "src/main.rs", FileStatus::Modified),
            change(0, "src/parser.rs", FileStatus::Conflicted),
            change(0, "README.md", FileStatus::Conflicted),
            change(0, "notes.md", FileStatus::Untracked),
        ];
        let rows = build_rows(&changes, &one_repo(), true, &HashSet::new(), &ClaudeGroups::default());
        assert_eq!(
            labels(&rows),
            vec![
                "Conflicts",
                "  src/",
                "    parser.rs",
                "  README.md",
                "Changes",
                "  src/",
                "    main.rs",
                "Unversioned",
                "  notes.md",
            ]
        );
        assert_eq!(rows[0].files.len(), 2);
        assert!(!checkable(&changes[1]) && checkable(&changes[0]));
    }

    fn claude_file(path: &str, seconds: u64) -> ClaudeFile {
        ClaudeFile {
            path: PathBuf::from(path),
            key: PathBuf::from(path),
            changed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(seconds),
        }
    }

    #[test]
    fn claude_changelists_take_their_files_and_the_later_session_wins() {
        let (first, second) = (EntityId::from(1u64), EntityId::from(2u64));
        let lists = vec![
            ClaudeList {
                session: first,
                title: "Fix login".into(),
                files: vec![
                    claude_file("/r0/src/auth.rs", 10),
                    claude_file("/r0/src/shared.rs", 30),
                    claude_file("/r0/src/parser.rs", 10),
                ],
            },
            ClaudeList {
                session: second,
                title: "Docs".into(),
                files: vec![
                    claude_file("/r0/README.md", 20),
                    claude_file("/r0/src/shared.rs", 20),
                    // Committed already: not among the changes, not shown.
                    claude_file("/r0/old.md", 20),
                ],
            },
        ];
        let groups = ClaudeGroups::build(&lists);
        let changes = vec![
            change(0, "src/auth.rs", FileStatus::Modified),
            change(0, "src/shared.rs", FileStatus::Modified),
            change(0, "README.md", FileStatus::Modified),
            change(0, "notes.md", FileStatus::Untracked),
            change(0, "main.rs", FileStatus::Modified),
            // Conflicted: it stays with the conflicts, whoever changed it.
            change(0, "src/parser.rs", FileStatus::Conflicted),
        ];
        let rows = build_rows(&changes, &one_repo(), true, &HashSet::new(), &groups);
        assert_eq!(
            labels(&rows),
            vec![
                "Conflicts",
                "  src/",
                "    parser.rs",
                "Claude 1",
                "  src/",
                "    auth.rs",
                "    shared.rs",
                "Claude 2",
                "  README.md",
                "Changes",
                "  main.rs",
                "Unversioned",
                "  notes.md",
            ]
        );
        assert_eq!(groups.title(second), Some("Docs"));
        // The Claude path and the canonical one both find the owner.
        let mut lists = lists;
        lists[1].files[0].key = PathBuf::from("/private/r0/README.md");
        let groups = ClaudeGroups::build(&lists);
        assert_eq!(groups.owner(&changes[2]), Some(second));
    }

    #[test]
    fn a_unified_diff_keeps_context_and_joins_close_blocks() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm\nn\n";
        let new = "a\nB\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm\nn\nO\n";
        let diff = unified_diff("x.txt", old, new);
        assert_eq!(
            diff,
            "--- a/x.txt\n+++ b/x.txt\n\
             @@ -1,5 +1,5 @@\n a\n-b\n+B\n c\n d\n e\n\
             @@ -12,3 +12,4 @@\n l\n m\n n\n+O\n"
        );
        // Blocks closer than twice the context share one header.
        let joined = unified_diff("x.txt", "1\n2\n3\n4\n5\n", "1\nX\n3\n4\nY\n");
        assert_eq!(joined.matches("@@ -").count(), 1);
        assert_eq!(unified_diff("x.txt", "same\r\n", "same\n"), "");
        // A new file: everything added.
        assert_eq!(
            unified_diff("new.rs", "", "fn main() {}"),
            "--- a/new.rs\n+++ b/new.rs\n@@ -1,0 +1,1 @@\n+fn main() {}\n"
        );
    }

    #[test]
    fn the_message_request_follows_the_style_and_cuts_a_long_diff() {
        let files = vec![
            MessageFile {
                relative: "src/a.rs".into(),
                status: FileStatus::Modified,
                old: "let x = 1;\n".into(),
                new: "let x = 2;\n".into(),
            },
            MessageFile {
                relative: "big.txt".into(),
                status: FileStatus::Untracked,
                old: String::new(),
                new: "line\n".repeat(MESSAGE_DIFF_LIMIT / 4),
            },
        ];
        let subjects = vec![
            "Этап 9.1: Claude Code — чат\n\nBody".to_string(),
            "README: снимки".to_string(),
        ];
        let prompt = message_prompt(&files, &subjects);
        assert!(prompt.contains("- Этап 9.1: Claude Code — чат\n- README: снимки\n"));
        assert!(!prompt.contains("Body"));
        assert!(prompt.contains("M src/a.rs\nA big.txt\n"));
        assert!(prompt.contains("-let x = 1;\n+let x = 2;\n"));
        assert!(prompt.contains("The diff of 1 more file(s) is left out"));
        assert!(message_prompt(&files[..1], &[]).contains("no commits yet"));
    }

    #[test]
    fn claudes_answer_loses_its_fence_and_quotes() {
        assert_eq!(clean_message("  Fix login\n\nBody  "), "Fix login\n\nBody");
        assert_eq!(clean_message("```text\nFix login\n```"), "Fix login");
        assert_eq!(clean_message("```\nFix\n```\n"), "Fix");
        assert_eq!(clean_message("\"Fix login\""), "Fix login");
        assert_eq!(clean_message("“Исправить вход”"), "Исправить вход");
        assert_eq!(clean_message("```"), "");
    }

    #[test]
    fn a_rollback_writes_the_text_before_claude() {
        let dir = std::env::temp_dir().join(format!("flux-claude-rollback-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.rs");
        std::fs::write(&path, "changed by Claude\n").unwrap();
        let missing = dir.join("gone.rs");
        let errors = restore_originals(&[
            (path.clone(), Some(Arc::from("before\r\n"))),
            // Created by Claude and already gone: nothing to do.
            (missing.clone(), None),
        ]);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "before\r\n");
        assert!(!missing.exists());
        // No temporary file is left next to it.
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_merge_is_committed_whole_and_without_conflicts() {
        use CheckState::*;
        let changes = vec![
            change(0, "a.rs", FileStatus::Modified),
            change(0, "b.rs", FileStatus::Added),
            change(0, "new.md", FileStatus::Untracked),
            change(1, "c.rs", FileStatus::Modified),
        ];
        // Not merging: anything goes.
        assert_eq!(
            merge_commit_problem(&changes, &[Partial, Unchecked, Unchecked, Unchecked], &[]),
            None
        );
        // Merging in repository 0: its tracked changes must be checked wholly; untracked files and
        // other repositories don't matter.
        assert_eq!(
            merge_commit_problem(&changes, &[Checked, Checked, Unchecked, Unchecked], &[0]),
            None
        );
        assert!(
            merge_commit_problem(&changes, &[Checked, Partial, Unchecked, Checked], &[0]).is_some()
        );
        assert!(
            merge_commit_problem(&changes, &[Unchecked, Checked, Checked, Checked], &[0]).is_some()
        );
        // A conflict left: resolve first.
        let mut conflicted = changes.clone();
        conflicted[3].status = FileStatus::Conflicted;
        assert!(
            merge_commit_problem(&conflicted, &[Checked, Checked, Unchecked, Checked], &[1])
                .is_some()
        );
        assert_eq!(
            merge_commit_problem(&conflicted, &[Checked, Checked, Unchecked, Checked], &[0]),
            None
        );
    }

    #[test]
    fn banners_say_what_is_in_progress() {
        let name_of = |oid: &str| (oid == "beef00001111").then(|| "origin/main".to_string());
        let merging = Operation {
            state: RepoState::Merging,
            incoming: Some("abc1234567".into()),
            incoming_name: Some("feature/x".into()),
            ..Default::default()
        };
        assert_eq!(
            banner_text(&merging, "main", name_of),
            Some((
                "Merging feature/x into main".to_string(),
                vec![BannerAction::Abort]
            ))
        );
        let rebasing = Operation {
            state: RepoState::Rebasing,
            rebase_branch: Some("main".into()),
            rebase_onto: Some("beef00001111".into()),
            step: Some((2, 5)),
            ..Default::default()
        };
        assert_eq!(
            banner_text(&rebasing, "1a2b3c4", name_of),
            Some((
                "Rebasing main onto origin/main · 2/5".to_string(),
                vec![
                    BannerAction::Continue,
                    BannerAction::Skip,
                    BannerAction::Abort
                ]
            ))
        );
        let picking = Operation {
            state: RepoState::CherryPicking,
            incoming: Some("0123456789".into()),
            ..Default::default()
        };
        assert_eq!(
            banner_text(&picking, "main", name_of).map(|(text, _)| text),
            Some("Cherry-picking 0123456".to_string())
        );
        assert_eq!(banner_text(&Operation::default(), "main", name_of), None);
    }

    #[test]
    fn nodes_aggregate_their_checkboxes() {
        use CheckState::*;
        assert_eq!(aggregate([Checked, Checked]), Checked);
        assert_eq!(aggregate([Unchecked, Unchecked]), Unchecked);
        assert_eq!(aggregate([Checked, Unchecked]), Partial);
        assert_eq!(aggregate([Checked, Partial]), Partial);
        assert_eq!(aggregate([]), Unchecked);
    }

    #[test]
    fn partial_content_keeps_only_included_hunks_and_crlf() {
        let base = "a\r\nb\r\nc\r\n";
        let current = "A\r\nb\r\nC\r\n";
        // Only the first change goes in.
        let content = partial_content(base, current, |hunk| hunk.old.start == 0);
        assert_eq!(content, "A\r\nb\r\nc\r\n");
        let content = partial_content("a\nb\n", "a\nb\nc\n", |_| false);
        assert_eq!(content, "a\nb\n");
    }

    #[test]
    fn history_items_are_first_lines() {
        assert_eq!(history_label("Fix parser\n\nLong body"), "Fix parser");
        let long = "x".repeat(80);
        assert_eq!(history_label(&long).chars().count(), HISTORY_ITEM_CHARS);
    }
}
