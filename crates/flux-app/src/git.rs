//! Git of a window: the repositories of the project, what changed in them, the HEAD versions of the
//! open documents, what the next commit includes, and the operations (commit, rollback, push) — the
//! hub every git feature reads from, as `LspStore` is for language servers.
//!
//! Repositories are found once per project root (`flux_git::find_repos`): the one containing the
//! root and those nested in it; a file belongs to the innermost one. Each repository is watched
//! (`flux_git::RepoWatcher`): changes are coalesced and `git status` runs in the background, one at a
//! time per repository (events that arrive meanwhile start one more run). A move of HEAD (a commit,
//! a checkout, a reset — here or in a terminal) drops the HEAD versions read so far, and the
//! registered editors get their new base.
//!
//! Branches, tags and stashes of each repository are read in the background and kept here (the
//! branches popup, the title bar's incoming / outgoing counts, the Stash tab), re-read when refs
//! change. The operations of part 6.2 — checkout, branches, merge, rebase, fetch, pull, update,
//! stash, conflicts — run here in the background, one at a time per repository; they return their
//! result and leave the reporting (notifications, questions) to the flows of the window.
//!
//! What the commit includes (the checkboxes of the commit window and of the diff viewer) lives here:
//! by default every tracked change is included and untracked files are not, as in JetBrains IDEs;
//! the user's choices are kept per file and, for a partial commit, per hunk (by its line range in
//! the HEAD version, which doesn't move while HEAD doesn't).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use flux_git::{
    BlobReader, CommitInfo, CommitRequest, CommitResult, ConflictKind, ConflictSide,
    ConflictVersions, DeletedBranch, FetchResult, FileChange, FileStatus, GitError, Hunk,
    Operation, Outcome, PullMode, PushProgress, PushRequest, PushResult, RefKind, Refs, Repo,
    RepoEvent, RepoState, RepoStatus, RepoWatcher, RollbackFile, Stash, StashRequest, UpdateMethod,
    UpdateResult,
};
use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    Action, App, AppContext, Context, Div, Entity, EventEmitter, Hsla, InteractiveElement,
    IntoElement, KeyBinding, NoAction, ParentElement, SharedString, StatefulInteractiveElement,
    Styled, Task, WeakEntity, actions, div, px,
};

use crate::editor::Editor;
use crate::i18n::{tr, trf};
use crate::icons::{IconName, icon};
use crate::notifications::Notification;
use crate::theme::UiColors;
use crate::workspace::Workspace;

actions!(
    git,
    [
        /// ⌘K: the commit window with focus in the message.
        Commit,
        /// ⇧⌘K: the push dialog.
        Push,
        /// ⌘0: show or hide the commit window.
        ToggleCommitWindow,
        /// ⌃V: the menu of version control operations.
        VcsOperations,
        /// The diff of the active file against HEAD.
        ShowDiff,
        /// Re-read the status of every repository.
        Refresh,
        /// ⇧⌘B: the branches popup.
        Branches,
        /// ⌘T outside a terminal: the current branches from their upstreams (merge or rebase).
        UpdateProject,
        /// The Pull dialog: a remote branch into the current one.
        Pull,
        /// Fetch every remote of every repository.
        Fetch,
        /// New Branch… from the current one.
        NewBranch,
        /// Checkout Tag or Revision…
        CheckoutRevision,
        /// The Stash Changes dialog.
        StashChanges,
        /// The Stash tab of the commit window.
        UnstashChanges,
        /// The Conflicts dialog: the conflicted files, Accept Yours / Theirs, Merge….
        ResolveConflicts,
        /// The operation in progress (a rebase, a cherry-pick…) goes on after the conflicts are
        /// resolved.
        ContinueOperation,
        /// The operation in progress is undone.
        AbortOperation,
        /// A rebase skips the commit it stopped on.
        SkipCommit,
        /// ⌘9: show or hide the Git window (the log) under the editor.
        ToggleGitWindow,
        /// ⌥⌘A: annotations (blame) of the active file in the gutter, on or off.
        Annotate,
        /// The history of the active file: a tab of the Git window.
        ShowFileHistory,
        /// The history of the selected lines of the active file.
        ShowSelectionHistory,
    ]
);

// --- Actions with data: the branches popup, notifications and banners dispatch them; the window
// handles them (`workspace_actions` of the module that owns the flow). Not in the palette. ---

/// Checkout of a branch, a remote branch (a new local one tracking it, or the existing one) or a
/// tag.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct CheckoutRef {
    pub repo: usize,
    pub name: String,
    pub kind: RefKind,
}

/// New Branch… from `start` (a branch, a tag, a commit).
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct NewBranchFrom {
    pub repo: usize,
    pub start: String,
}

/// Rename… of a local branch.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct RenameBranch {
    pub repo: usize,
    pub name: String,
}

/// Delete of a branch (local or remote) or a tag, with the questions it needs.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct DeleteRef {
    pub repo: usize,
    pub name: String,
    pub kind: RefKind,
}

/// Restore of a deleted branch (or tag) at its commit: the notification after a delete.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct RestoreRef {
    pub repo: usize,
    pub name: String,
    pub oid: String,
    pub kind: RefKind,
}

/// Merge `name` into the current branch.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct MergeRef {
    pub repo: usize,
    pub name: String,
}

/// Rebase the current branch onto `onto`.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct RebaseOnto {
    pub repo: usize,
    pub onto: String,
}

/// Checkout `branch` and rebase it onto `onto` (the current branch).
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct CheckoutAndRebase {
    pub repo: usize,
    pub branch: String,
    pub onto: String,
}

/// Pull a remote branch ("origin/feature/x") into the current one.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct PullRef {
    pub repo: usize,
    pub name: String,
    pub rebase: bool,
}

/// Update of a local branch: the current one — Update Project for its repository; another one — a
/// fast-forward from its upstream.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct UpdateBranch {
    pub repo: usize,
    pub name: String,
}

/// Push… of a local branch (the push dialog for it).
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct PushBranch {
    pub repo: usize,
    pub name: String,
}

/// Compare with Current: the commits and the files that differ from the current branch.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct CompareWithCurrent {
    pub repo: usize,
    pub name: String,
}

/// Show Diff with Working Tree: the files that differ from a revision.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct DiffWithWorkingTree {
    pub repo: usize,
    pub name: String,
}

/// Continue / abort / skip in one repository (the commit window's banner).
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct ContinueRepoOperation {
    pub repo: usize,
}

#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct AbortRepoOperation {
    pub repo: usize,
}

#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct SkipRepoCommit {
    pub repo: usize,
}

/// The merge tool for a conflicted file.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct OpenMerge {
    pub repo: usize,
    pub path: PathBuf,
}

/// Drop a stash (the notification after an unstash that conflicted).
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct DropStash {
    pub repo: usize,
    pub oid: String,
}

/// A diff tab comparing a file at two revisions (or a revision and the working copy): stash
/// files, Compare with Current, Show Diff with Working Tree.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct OpenCompareDiff {
    pub repo: usize,
    /// The file (absolute) as the working tree has it.
    pub path: PathBuf,
    pub left: crate::diff_view::DiffSide,
    pub right: crate::diff_view::DiffSide,
}

// --- Actions of part 6.3: the log, the commit pane, operations on commits, rewriting history,
// annotations. `oids` of several commits are newest first, as the log shows them. ---

/// The Git window with the log, the commit selected in it (annotations, notifications).
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct ShowCommitInLog {
    pub repo: usize,
    pub oid: String,
}

/// The history of a file (absolute path), or of its lines `start..=end` (1-based): a tab of the
/// Git window.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct ShowHistory {
    pub path: PathBuf,
    pub lines: Option<(u32, u32)>,
}

/// Checkout of a commit: a detached HEAD, with a warning.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct CheckoutCommit {
    pub repo: usize,
    pub oid: String,
}

/// New Tag… at a commit.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct NewTagAt {
    pub repo: usize,
    pub oid: String,
}

/// Cherry-pick commits onto the current branch.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct CherryPick {
    pub repo: usize,
    pub oids: Vec<String>,
}

/// Revert commits.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct RevertCommits {
    pub repo: usize,
    pub oids: Vec<String>,
}

/// Reset Current Branch to Here…: the dialog with soft / mixed / hard / keep.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct ResetToCommit {
    pub repo: usize,
    pub oid: String,
}

/// Undo Commit: the last commit goes back to the changes (not pushed yet).
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct UndoCommit {
    pub repo: usize,
    pub oid: String,
}

/// Edit Commit Message…
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct EditCommitMessage {
    pub repo: usize,
    pub oid: String,
}

/// Fixup… / Squash Into…: `oids` meld into `target` (fixup keeps its message, squash asks for
/// one).
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct MeldCommits {
    pub repo: usize,
    pub target: String,
    pub oids: Vec<String>,
    pub squash: bool,
}

/// Drop Commits.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct DropCommits {
    pub repo: usize,
    pub oids: Vec<String>,
}

/// Interactively Rebase from Here…: the plan dialog from this commit up to HEAD.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct InteractiveRebase {
    pub repo: usize,
    pub oid: String,
}

/// git's whole output in a dialog (an error notification's "Details").
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct ShowGitOutput {
    pub title: String,
    pub output: String,
}

/// Watcher events are coalesced for at least this long before `git status` runs; a repository whose
/// status is slow waits twice its last run, up to the maximum.
const REFRESH_DELAY: Duration = Duration::from_millis(100);
const MAX_REFRESH_DELAY: Duration = Duration::from_secs(2);
/// After this many refreshes in a row that changed nothing, the ignored set is read again…
const IDLE_REFRESHES_BEFORE_IGNORED: u32 = 3;
/// …but not more often than this.
const IGNORED_REREAD_INTERVAL: Duration = Duration::from_secs(5);
/// Larger documents get no gutter markers: their HEAD version isn't read.
const MAX_BASE_BYTES: usize = 4 * 1024 * 1024;

pub fn init(cx: &mut App) {
    let workspace = Some("Workspace");
    cx.bind_keys([
        // As in JetBrains IDEs (macOS keymap).
        KeyBinding::new("cmd-k", Commit, workspace),
        KeyBinding::new("cmd-shift-k", Push, workspace),
        KeyBinding::new("cmd-0", ToggleCommitWindow, workspace),
        KeyBinding::new("ctrl-v", VcsOperations, workspace),
        // The author's own key: JetBrains has none for the branches popup on macOS.
        KeyBinding::new("cmd-shift-b", Branches, workspace),
        // In a terminal, ⌘T is a new terminal (`terminal_panel`), as in JetBrains IDEs.
        KeyBinding::new("cmd-t", UpdateProject, workspace),
        KeyBinding::new("cmd-9", ToggleGitWindow, workspace),
        // The author's own key: JetBrains has none for Annotate on macOS.
        KeyBinding::new("alt-cmd-a", Annotate, workspace),
        // Programs in a terminal need ⌃V (vim's visual block, a literal next character).
        KeyBinding::new("ctrl-v", NoAction, Some("Terminal")),
    ]);
}

/// What changed, for those who watch the store.
#[derive(Debug, Clone)]
pub enum GitEvent {
    /// A message for the status bar ("Committed 3 files").
    Message(SharedString),
    /// An operation failed: a short message and git's whole output (hook output, a rejected push)
    /// for a dialog.
    Error {
        message: SharedString,
        details: Option<String>,
    },
    /// A notification in the corner of the window (an operation's result, with actions).
    Notify(Notification),
    /// An operation of the store changed files of a repository's working tree (checkout, merge,
    /// stash…): open documents take the new content, those whose files are gone close.
    WorkTreeChanged(usize),
}

/// A changed file of one of the repositories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// Index in [`GitStore::repos`].
    pub repo: usize,
    pub path: PathBuf,
    /// Relative to the repository's working tree, with `/`.
    pub relative: String,
    /// The path before a rename.
    pub orig_path: Option<PathBuf>,
    pub status: FileStatus,
    /// How a conflicted file conflicts.
    pub conflict: Option<ConflictKind>,
}

/// A file's checkbox: in the commit wholly, partly (some of its hunks), or not at all.
pub use crate::ui::CheckState;

/// What the next commit includes: the user's choices over the defaults (a tracked change is in, an
/// untracked file is out — the caller passes the default of each file). Hunks are keyed by their
/// line range in the HEAD version, which stays put while HEAD does.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Inclusion {
    included: HashMap<PathBuf, bool>,
    excluded_hunks: HashMap<PathBuf, HashSet<(u32, u32)>>,
}

fn hunk_key(hunk: &Hunk) -> (u32, u32) {
    (hunk.old.start, hunk.old.end)
}

impl Inclusion {
    pub fn file_included(&self, path: &Path, default: bool) -> bool {
        self.included.get(path).copied().unwrap_or(default)
    }

    /// The file's checkbox.
    pub fn state(&self, path: &Path, default: bool) -> CheckState {
        if !self.file_included(path, default) {
            CheckState::Unchecked
        } else if self
            .excluded_hunks
            .get(path)
            .is_some_and(|hunks| !hunks.is_empty())
        {
            CheckState::Partial
        } else {
            CheckState::Checked
        }
    }

    /// Checks or unchecks a whole file: its hunk choices go.
    pub fn set_file(&mut self, path: &Path, included: bool) {
        self.included.insert(path.to_path_buf(), included);
        self.excluded_hunks.remove(path);
    }

    pub fn hunk_included(&self, path: &Path, hunk: &Hunk, default: bool) -> bool {
        self.file_included(path, default)
            && !self
                .excluded_hunks
                .get(path)
                .is_some_and(|hunks| hunks.contains(&hunk_key(hunk)))
    }

    /// Checks or unchecks one hunk; `all` are the file's hunks. Including a hunk of an unchecked
    /// file includes the file with only that hunk; excluding the last included hunk unchecks the
    /// file; including the last excluded one makes it whole again.
    pub fn set_hunk(
        &mut self,
        path: &Path,
        hunk: &Hunk,
        included: bool,
        all: &[Hunk],
        default: bool,
    ) {
        let file_included = all
            .iter()
            .any(|hunk| self.hunk_included(path, hunk, default));
        let excluded = self.excluded_hunks.entry(path.to_path_buf()).or_default();
        if included && !file_included {
            excluded.extend(all.iter().map(hunk_key));
        }
        if included {
            excluded.remove(&hunk_key(hunk));
        } else {
            excluded.insert(hunk_key(hunk));
        }
        // Choices about hunks that no longer exist don't count.
        excluded.retain(|key| all.iter().any(|hunk| hunk_key(hunk) == *key));
        let none_left =
            !all.is_empty() && all.iter().all(|hunk| excluded.contains(&hunk_key(hunk)));
        let all_in = excluded.is_empty();
        if none_left {
            self.excluded_hunks.remove(path);
            self.included.insert(path.to_path_buf(), false);
        } else {
            if all_in {
                self.excluded_hunks.remove(path);
            }
            self.included.insert(path.to_path_buf(), true);
        }
    }

    /// Keeps the choices about the files `keep` says yes to.
    pub fn retain(&mut self, keep: impl Fn(&Path) -> bool) {
        self.included.retain(|path, _| keep(path));
        self.excluded_hunks.retain(|path, _| keep(path));
    }

    /// HEAD moved in a repository: its hunk keys mean other lines now.
    pub fn forget_hunks_under(&mut self, dir: &Path) {
        self.excluded_hunks.retain(|path, _| !path.starts_with(dir));
    }
}

/// A repository of the window.
pub struct GitRepo {
    pub repo: Repo,
    /// The latest `git status`; empty until the first one comes.
    pub status: Arc<RepoStatus>,
    blobs: Arc<BlobReader>,
    /// HEAD versions read so far, by relative path (`None`: not in HEAD, binary or too large).
    bases: HashMap<String, Option<Arc<str>>>,
    /// HEAD when the bases were read: a new one drops them.
    bases_head: Option<String>,
    /// `git status` is running; `stale` — events came meanwhile, run again after it.
    refreshing: bool,
    stale: bool,
    /// How long the last `git status` took: the watcher waits twice that between runs, so a huge
    /// repository isn't re-read back to back while a build writes files.
    last_duration: Duration,
    /// Ignored files and directories (absolute): changes inside them don't start a refresh.
    ignored: Arc<HashSet<PathBuf>>,
    ignored_reading: bool,
    ignored_read_at: Option<Instant>,
    /// Refreshes in a row that changed nothing: after a few, the ignored set is read again (a build
    /// made a new ignored directory).
    idle_refreshes: u32,
    /// Wholly untracked directories (absolute).
    untracked_dirs: Arc<Vec<PathBuf>>,
    /// The operation in progress (merge, rebase…), read with every status.
    pub operation: Arc<Operation>,
    /// Branches, tags, remotes and recent branches; empty until the first read.
    pub refs: Arc<Refs>,
    refs_reading: bool,
    refs_stale: bool,
    /// The stashes, once someone asked for them (the Stash tab); re-read when refs change.
    stashes: Option<Arc<Vec<Stash>>>,
    stashes_reading: bool,
    /// An operation of the store runs in the repository: another one waits for it to end.
    busy: bool,
    _watcher: Option<RepoWatcher>,
    _watch_task: Task<()>,
}

impl GitRepo {
    /// Whether a path of the working tree is ignored, or inside an ignored directory.
    fn is_ignored(&self, path: &Path) -> bool {
        path.ancestors()
            .take_while(|dir| *dir != self.repo.work_dir && dir.starts_with(&self.repo.work_dir))
            .any(|dir| self.ignored.contains(dir))
    }
}

/// Lookups over the statuses of all repositories, rebuilt when a status changes: the tree, the
/// tabs, the launchpad and the commit window ask every frame.
#[derive(Default)]
struct StatusIndex {
    /// Changed files of every repository, by repository and path.
    changes: Vec<Change>,
    statuses: HashMap<PathBuf, FileStatus>,
    /// Directories with a tracked change inside, up to the project root (or the repository's top,
    /// for a repository outside the root).
    changed_dirs: HashSet<PathBuf>,
    untracked_dirs: HashSet<PathBuf>,
}

impl StatusIndex {
    fn dir_status(&self, dir: &Path) -> Option<FileStatus> {
        let untracked = !self.untracked_dirs.is_empty()
            && dir.ancestors().any(|dir| self.untracked_dirs.contains(dir));
        if untracked {
            Some(FileStatus::Untracked)
        } else if self.changed_dirs.contains(dir) {
            Some(FileStatus::Modified)
        } else {
            None
        }
    }

    /// The lookups over the repositories' statuses and their wholly untracked directories.
    fn build<'a>(
        root: Option<&Path>,
        repos: impl Iterator<Item = (&'a Repo, &'a RepoStatus, &'a [PathBuf])>,
    ) -> Self {
        let mut index = StatusIndex::default();
        for (repo_index, (repo, status, untracked_dirs)) in repos.enumerate() {
            for change in &status.entries {
                let path = repo.absolute(&change.path);
                index.statuses.insert(path.clone(), change.status);
                if change.status != FileStatus::Untracked {
                    let stop = match root {
                        Some(root) if path.starts_with(root) => root,
                        _ => repo.work_dir.as_path(),
                    };
                    for dir in path.ancestors().skip(1) {
                        // Directories are added deepest first: a known one has its parents too.
                        if !dir.starts_with(stop) || !index.changed_dirs.insert(dir.to_path_buf()) {
                            break;
                        }
                    }
                }
                index.changes.push(Change {
                    repo: repo_index,
                    path,
                    relative: change.path.clone(),
                    orig_path: change.orig_path.as_deref().map(|orig| repo.absolute(orig)),
                    status: change.status,
                    conflict: change.conflict,
                });
            }
            index.untracked_dirs.extend(untracked_dirs.iter().cloned());
        }
        index
    }
}

/// The git hub of a window; recreated with the project root.
pub struct GitStore {
    root: Option<PathBuf>,
    repos: Vec<GitRepo>,
    index: StatusIndex,
    /// Repositories are being looked for (in the background).
    discovering: bool,
    /// The branch read from `.git/HEAD` when the store was made: the title bar shows it until the
    /// first status arrives.
    initial_branch: Option<SharedString>,
    /// Registered editors: they get the HEAD version of their document.
    editors: Vec<WeakEntity<Editor>>,
    registered: HashSet<gpui::EntityId>,
    /// What the commit includes: the user's checkbox choices.
    inclusion: Inclusion,
    /// The operation in progress, for the status bar: "Committing…", "Pushing 45%".
    activity: Option<SharedString>,
    /// Favorite branches of the repositories (the branches popup's stars).
    favorites: Favorites,
}

impl EventEmitter<GitEvent> for GitStore {}

impl GitStore {
    pub fn new(root: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        let initial_branch = root.as_deref().and_then(read_branch);
        let mut store = Self {
            root: root.clone(),
            repos: Vec::new(),
            index: StatusIndex::default(),
            discovering: root.is_some(),
            initial_branch,
            editors: Vec::new(),
            registered: HashSet::new(),
            inclusion: Inclusion::default(),
            activity: None,
            favorites: Favorites::load(),
        };
        if let Some(root) = root {
            store.discover(root, cx);
        }
        store
    }

    fn discover(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let found = cx.background_spawn(async move { flux_git::find_repos(&root) });
        cx.spawn(async move |this, cx| {
            let repos = found.await;
            this.update(cx, |this, cx| {
                this.discovering = false;
                for repo in repos {
                    this.add_repo(repo, cx);
                }
                // Editors registered before the repositories were known.
                for editor in this.editors.clone() {
                    if let Some(editor) = editor.upgrade() {
                        this.load_base(&editor, cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn add_repo(&mut self, repo: Repo, cx: &mut Context<Self>) {
        let index = self.repos.len();
        let (sender, mut events) = mpsc::unbounded::<RepoEvent>();
        let watcher = RepoWatcher::new(&repo, move |event| {
            sender.unbounded_send(event).ok();
        })
        .map_err(|err| eprintln!("flux: not watching {}: {err}", repo.work_dir.display()))
        .ok();
        let watch_task = cx.spawn(async move |this, cx| {
            while let Some(first) = events.next().await {
                let Ok(delay) = this.update(cx, |this, _| this.refresh_delay(index)) else {
                    break;
                };
                cx.background_executor().timer(delay).await;
                let mut batch = vec![first];
                while let Ok(event) = events.try_recv() {
                    batch.push(event);
                }
                if this
                    .update(cx, |this, cx| this.on_events(index, batch, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        self.repos.push(GitRepo {
            blobs: Arc::new(BlobReader::new(&repo)),
            repo,
            status: Arc::default(),
            bases: HashMap::new(),
            bases_head: None,
            refreshing: false,
            stale: false,
            last_duration: Duration::ZERO,
            ignored: Arc::default(),
            ignored_reading: false,
            ignored_read_at: None,
            idle_refreshes: 0,
            untracked_dirs: Arc::default(),
            operation: Arc::default(),
            refs: Arc::default(),
            refs_reading: false,
            refs_stale: false,
            stashes: None,
            stashes_reading: false,
            busy: false,
            _watcher: watcher,
            _watch_task: watch_task,
        });
        self.read_ignored(index, cx);
        self.refresh_repo(index, cx);
        self.reload_refs(index, cx);
    }

    pub fn repos(&self) -> &[GitRepo] {
        &self.repos
    }

    /// Repositories are still being looked for.
    pub fn is_discovering(&self) -> bool {
        self.discovering
    }

    /// The index of the innermost repository containing `path`.
    pub fn repo_index(&self, path: &Path) -> Option<usize> {
        self.repos
            .iter()
            .enumerate()
            .filter(|(_, repo)| repo.repo.contains(path))
            .max_by_key(|(_, repo)| repo.repo.work_dir.components().count())
            .map(|(index, _)| index)
    }

    pub fn repo_for(&self, path: &Path) -> Option<&GitRepo> {
        self.repo_index(path).map(|index| &self.repos[index])
    }

    // --- Status ---

    /// Re-reads the status of every repository.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        for index in 0..self.repos.len() {
            self.refresh_repo(index, cx);
        }
    }

    /// How long the watcher gathers events before a refresh: longer for a repository whose status
    /// is slow.
    fn refresh_delay(&self, index: usize) -> Duration {
        let last = self
            .repos
            .get(index)
            .map_or(Duration::ZERO, |entry| entry.last_duration);
        (last * 2).clamp(REFRESH_DELAY, MAX_REFRESH_DELAY)
    }

    /// A batch of watcher events: changes inside ignored paths don't matter; a changed `.gitignore`
    /// re-reads the ignored set.
    fn on_events(&mut self, index: usize, batch: Vec<RepoEvent>, cx: &mut Context<Self>) {
        let Some(entry) = self.repos.get(index) else {
            return;
        };
        let mut refresh = false;
        let mut rules = false;
        let mut refs = false;
        let mut skipped = 0;
        for event in &batch {
            match event {
                RepoEvent::Git | RepoEvent::Rescan => {
                    refresh = true;
                    refs = true;
                }
                RepoEvent::WorkTree(paths) => {
                    for path in paths {
                        if path
                            .file_name()
                            .is_some_and(|name| name == ".gitignore" || name == ".ignore")
                        {
                            rules = true;
                            refresh = true;
                        } else if entry.is_ignored(path) {
                            skipped += 1;
                        } else {
                            refresh = true;
                        }
                    }
                }
            }
        }
        if skipped > 0 {
            log(|| format!("skipped {skipped} changes in ignored paths"));
        }
        if rules {
            self.read_ignored(index, cx);
        }
        if refresh {
            self.refresh_repo(index, cx);
        }
        if refs {
            self.reload_refs(index, cx);
        }
    }

    /// Reads the ignored files and directories of a repository, in the background.
    fn read_ignored(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.repos.get_mut(index) else {
            return;
        };
        if entry.ignored_reading {
            return;
        }
        entry.ignored_reading = true;
        let repo = entry.repo.clone();
        let read = cx.background_spawn(async move {
            let paths = flux_git::ignored_paths(&repo).unwrap_or_default();
            paths
                .iter()
                .map(|path| repo.absolute(path))
                .collect::<HashSet<PathBuf>>()
        });
        cx.spawn(async move |this, cx| {
            let ignored = read.await;
            this.update(cx, |this, _| {
                if let Some(entry) = this.repos.get_mut(index) {
                    log(|| format!("{} ignored paths", ignored.len()));
                    entry.ignored = Arc::new(ignored);
                    entry.ignored_reading = false;
                    entry.ignored_read_at = Some(Instant::now());
                }
            })
            .ok();
        })
        .detach();
    }

    fn refresh_repo(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.repos.get_mut(index) else {
            return;
        };
        if entry.refreshing {
            entry.stale = true;
            return;
        }
        entry.refreshing = true;
        entry.stale = false;
        let repo = entry.repo.clone();
        let previous = entry.status.clone();
        let previous_dirs = entry.untracked_dirs.clone();
        let first = entry.status.branch == Default::default() && entry.status.entries.is_empty();
        let read = cx.background_spawn(async move {
            let started = Instant::now();
            let status = flux_git::status(&repo)?;
            let operation = flux_git::operation(&repo);
            let untracked = |status: &RepoStatus| -> Vec<String> {
                status
                    .entries
                    .iter()
                    .filter(|entry| entry.status == FileStatus::Untracked)
                    .map(|entry| entry.path.clone())
                    .collect()
            };
            // The untracked directories change only with the untracked files.
            let dirs = if !first && untracked(&status) == untracked(&previous) {
                previous_dirs
            } else {
                let dirs = flux_git::untracked_dirs(&repo).unwrap_or_default();
                Arc::new(dirs.iter().map(|dir| repo.absolute(dir)).collect())
            };
            Ok::<_, GitError>((status, dirs, operation, started.elapsed()))
        });
        cx.spawn(async move |this, cx| {
            let result = read.await;
            this.update(cx, |this, cx| {
                let Some(entry) = this.repos.get_mut(index) else {
                    return;
                };
                entry.refreshing = false;
                let (changed, head_moved) = match result {
                    Ok((status, dirs, operation, took)) => {
                        log(|| {
                            format!(
                                "status of {}: {} ms, {} entries",
                                entry.repo.work_dir.display(),
                                took.as_millis(),
                                status.entries.len()
                            )
                        });
                        entry.last_duration = took;
                        let head_moved = entry.bases_head != status.branch.oid;
                        let operation_changed = *entry.operation != operation;
                        if operation_changed {
                            entry.operation = Arc::new(operation);
                        }
                        let changed = *entry.status != status
                            || entry.untracked_dirs != dirs
                            || operation_changed;
                        if changed {
                            entry.status = Arc::new(status);
                            entry.untracked_dirs = dirs;
                            entry.idle_refreshes = 0;
                        } else {
                            entry.idle_refreshes += 1;
                        }
                        (changed, head_moved)
                    }
                    Err(err) => {
                        eprintln!("flux: git status: {err}");
                        (false, false)
                    }
                };
                // Refreshes that change nothing: perhaps a build made a new ignored directory.
                let reread_ignored = entry.idle_refreshes >= IDLE_REFRESHES_BEFORE_IGNORED
                    && entry
                        .ignored_read_at
                        .is_none_or(|at| at.elapsed() >= IGNORED_REREAD_INTERVAL);
                if reread_ignored {
                    entry.idle_refreshes = 0;
                    this.read_ignored(index, cx);
                }
                if changed {
                    this.rebuild_index();
                    cx.notify();
                }
                if head_moved {
                    this.head_moved(index, cx);
                }
                if this.repos.get(index).is_some_and(|entry| entry.stale) {
                    this.refresh_repo(index, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Rebuilds the lookups after a status changed; choices about files that are no longer changed
    /// are forgotten.
    fn rebuild_index(&mut self) {
        let index = StatusIndex::build(
            self.root.as_deref(),
            self.repos
                .iter()
                .map(|entry| (&entry.repo, &*entry.status, entry.untracked_dirs.as_slice())),
        );
        let statuses = &index.statuses;
        self.inclusion.retain(|path| statuses.contains_key(path));
        self.index = index;
    }

    /// HEAD moved: the HEAD versions read so far are stale; registered editors in the repository get
    /// their new base.
    fn head_moved(&mut self, index: usize, cx: &mut Context<Self>) {
        let entry = &mut self.repos[index];
        entry.bases.clear();
        entry.bases_head = entry.status.branch.oid.clone();
        let work_dir = entry.repo.work_dir.clone();
        self.inclusion.forget_hunks_under(&work_dir);
        for editor in self.editors.clone() {
            let Some(editor) = editor.upgrade() else {
                continue;
            };
            let inside = editor
                .read(cx)
                .document
                .path()
                .is_some_and(|path| path.starts_with(&work_dir));
            if inside {
                self.load_base(&editor, cx);
            }
        }
    }

    /// The change of a file against HEAD; `None` — unchanged, ignored, or not in a repository.
    pub fn status_of(&self, path: &Path) -> Option<FileStatus> {
        self.index.statuses.get(path).copied()
    }

    /// The color class of a directory in the tree: untracked as a whole (`Untracked`), with a
    /// tracked change inside (`Modified`), or neither.
    pub fn dir_status(&self, dir: &Path) -> Option<FileStatus> {
        self.index.dir_status(dir)
    }

    /// Every changed file of every repository, by repository and path.
    pub fn changes(&self) -> Vec<Change> {
        self.index.changes.clone()
    }

    /// How many files changed.
    pub fn change_count(&self) -> usize {
        self.index.changes.len()
    }

    /// The branch for the title bar: of the repository of `path` (the active file), otherwise of the
    /// one containing the project root, otherwise of the first one.
    pub fn branch_label(&self, path: Option<&Path>) -> Option<SharedString> {
        let entry = path
            .and_then(|path| self.repo_for(path))
            .or_else(|| self.root.as_deref().and_then(|root| self.repo_for(root)))
            .or_else(|| self.repos.first());
        match entry {
            Some(entry) if entry.status.branch != Default::default() => {
                entry.status.branch.label().map(Into::into)
            }
            _ => self.initial_branch.clone(),
        }
    }

    // --- HEAD versions ---

    /// The HEAD version of a file (absolute path): `None` — not in a repository, not in HEAD,
    /// binary, or too large. Read once per HEAD, in the background.
    pub fn base_text(&mut self, path: &Path, cx: &mut Context<Self>) -> Task<Option<Arc<str>>> {
        let Some(index) = self.repo_index(path) else {
            return Task::ready(None);
        };
        let entry = &self.repos[index];
        let Some(relative) = entry.repo.relative(path) else {
            return Task::ready(None);
        };
        if let Some(base) = entry.bases.get(&relative) {
            return Task::ready(base.clone());
        }
        let blobs = entry.blobs.clone();
        let head = entry.bases_head.clone();
        let read = cx.background_spawn({
            let relative = relative.clone();
            async move {
                let content = blobs.read("HEAD", &relative).ok().flatten()?;
                if content.len() > MAX_BASE_BYTES || flux_git::is_binary(&content) {
                    return None;
                }
                Some(Arc::<str>::from(String::from_utf8_lossy(&content).as_ref()))
            }
        });
        cx.spawn(async move |this, cx| {
            let base = read.await;
            this.update(cx, |this, _| {
                // Keep it only if HEAD didn't move while it was being read.
                if let Some(entry) = this.repos.get_mut(index)
                    && entry.bases_head == head
                {
                    entry.bases.insert(relative, base.clone());
                }
            })
            .ok();
            base
        })
    }

    /// The editor's document gets its HEAD version now and whenever HEAD moves or its path changes.
    pub fn register(&mut self, editor: &Entity<Editor>, cx: &mut Context<Self>) {
        let id = editor.entity_id();
        if self.registered.insert(id) {
            self.editors.push(editor.downgrade());
            cx.observe_release(editor, move |this, _, _| {
                this.registered.remove(&id);
                this.editors.retain(|editor| editor.entity_id() != id);
            })
            .detach();
        }
        let store = cx.weak_entity();
        editor.update(cx, |editor, _| editor.git.store = Some(store));
        self.load_base(editor, cx);
    }

    /// The registered editors that are still alive: the documents of the window.
    pub fn editors(&self) -> Vec<Entity<Editor>> {
        self.editors
            .iter()
            .filter_map(WeakEntity::upgrade)
            .collect()
    }

    /// Reads the HEAD version of the editor's document and hands it over.
    pub fn load_base(&mut self, editor: &Entity<Editor>, cx: &mut Context<Self>) {
        let path = editor.read(cx).document.path().map(Path::to_path_buf);
        let base = match &path {
            Some(path) => self.base_text(path, cx),
            None => Task::ready(None),
        };
        let editor = editor.downgrade();
        cx.spawn(async move |_, cx| {
            let base = base.await;
            editor
                .update(cx, |editor, cx| {
                    // The path may have changed while the base was read.
                    if editor.document.path() == path.as_deref() {
                        crate::git_gutter::set_base(editor, base, cx);
                    }
                })
                .ok();
        })
        .detach();
    }

    // --- What the commit includes ---

    /// Whether a file is in the commit by default: tracked changes are, untracked files are not.
    fn included_by_default(&self, path: &Path) -> bool {
        self.status_of(path)
            .is_some_and(|status| status != FileStatus::Untracked)
    }

    /// The checkbox of a changed file.
    pub fn check_state(&self, change: &Change) -> CheckState {
        self.inclusion
            .state(&change.path, change.status != FileStatus::Untracked)
    }

    /// Checks or unchecks a whole file.
    pub fn set_included(&mut self, path: &Path, included: bool, cx: &mut Context<Self>) {
        self.inclusion.set_file(path, included);
        cx.notify();
    }

    /// Whether a hunk of a file is in the commit.
    pub fn is_hunk_included(&self, path: &Path, hunk: &Hunk) -> bool {
        self.inclusion
            .hunk_included(path, hunk, self.included_by_default(path))
    }

    /// Checks or unchecks one hunk; `all` are the file's hunks (see [`Inclusion::set_hunk`]).
    pub fn set_hunk_included(
        &mut self,
        path: &Path,
        hunk: &Hunk,
        included: bool,
        all: &[Hunk],
        cx: &mut Context<Self>,
    ) {
        let default = self.included_by_default(path);
        self.inclusion.set_hunk(path, hunk, included, all, default);
        cx.notify();
    }

    // --- Operations ---

    /// The operation in progress, for the status bar.
    pub fn activity(&self) -> Option<&SharedString> {
        self.activity.as_ref()
    }

    fn set_activity(&mut self, activity: Option<SharedString>, cx: &mut Context<Self>) {
        self.activity = activity;
        cx.notify();
    }

    /// Commits in one repository; the status is re-read after.
    pub fn commit(
        &mut self,
        repo: usize,
        request: CommitRequest,
        cx: &mut Context<Self>,
    ) -> Task<Result<CommitResult, GitError>> {
        let Some(entry) = self.repos.get(repo) else {
            return Task::ready(Err(GitError::Canceled));
        };
        let git_repo = entry.repo.clone();
        self.set_activity(Some(tr("Committing…").into()), cx);
        let run = cx.background_spawn(async move {
            flux_git::commit::commit(&git_repo, &request, &flux_git::Cancel::new())
        });
        cx.spawn(async move |this, cx| {
            let result = run.await;
            this.update(cx, |this, cx| {
                this.set_activity(None, cx);
                this.refresh_repo(repo, cx);
            })
            .ok();
            result
        })
    }

    /// Rolls files (absolute paths, with their status) back to HEAD; added files stay on disk
    /// unless `delete_added`.
    pub fn rollback(
        &mut self,
        changes: Vec<Change>,
        delete_added: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), GitError>> {
        let mut by_repo: HashMap<usize, Vec<RollbackFile>> = HashMap::new();
        for change in changes {
            let Some(entry) = self.repos.get(change.repo) else {
                continue;
            };
            by_repo.entry(change.repo).or_default().push(RollbackFile {
                path: change.relative,
                status: change.status,
                orig_path: change
                    .orig_path
                    .as_deref()
                    .and_then(|orig| entry.repo.relative(orig)),
            });
        }
        let jobs: Vec<(Repo, Vec<RollbackFile>)> = by_repo
            .into_iter()
            .filter_map(|(index, files)| Some((self.repos.get(index)?.repo.clone(), files)))
            .collect();
        self.set_activity(Some(tr("Rolling back…").into()), cx);
        let run = cx.background_spawn(async move {
            for (repo, files) in jobs {
                flux_git::rollback(&repo, &files, delete_added)?;
            }
            Ok(())
        });
        cx.spawn(async move |this, cx| {
            let result = run.await;
            this.update(cx, |this, cx| {
                this.set_activity(None, cx);
                this.refresh(cx);
            })
            .ok();
            result
        })
    }

    /// Pushes from one repository; the progress goes to the status bar.
    pub fn push(
        &mut self,
        repo: usize,
        request: PushRequest,
        cx: &mut Context<Self>,
    ) -> Task<Result<PushResult, GitError>> {
        let Some(entry) = self.repos.get(repo) else {
            return Task::ready(Err(GitError::Canceled));
        };
        let git_repo = entry.repo.clone();
        self.set_activity(Some(tr("Pushing…").into()), cx);
        let (sender, mut progress) = mpsc::unbounded::<PushProgress>();
        let run = cx.background_spawn(async move {
            flux_git::push::push(
                &git_repo,
                &request,
                move |step| {
                    sender.unbounded_send(step).ok();
                },
                &flux_git::Cancel::new(),
            )
        });
        let watch = cx.spawn(async move |this, cx| {
            while let Some(step) = progress.next().await {
                let text = match step.percent {
                    Some(percent) => trf("Pushing… {0}%", &[&percent]),
                    None => tr("Pushing…").to_string(),
                };
                if this
                    .update(cx, |this, cx| this.set_activity(Some(text.into()), cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        cx.spawn(async move |this, cx| {
            let result = run.await;
            drop(watch);
            this.update(cx, |this, cx| {
                this.set_activity(None, cx);
                this.refresh_repo(repo, cx);
            })
            .ok();
            result
        })
    }

    /// Adds a file or directory to the `.gitignore` of its repository.
    pub fn add_to_gitignore(
        &mut self,
        path: &Path,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let Some(entry) = self.repo_for(path) else {
            return Task::ready(Err(tr("Not in a Git repository").to_string()));
        };
        let repo = entry.repo.clone();
        let Some(relative) = repo.relative(path) else {
            return Task::ready(Err(tr("Not in a Git repository").to_string()));
        };
        let is_dir = path.is_dir();
        cx.background_spawn(async move {
            flux_git::add_to_gitignore(&repo, &relative, is_dir).map_err(|err| err.to_string())
        })
    }

    /// Tells the window about a finished operation.
    pub fn report(&mut self, event: GitEvent, cx: &mut Context<Self>) {
        cx.emit(event);
    }

    /// A notification in the corner of the window.
    pub fn notify(&mut self, notification: Notification, cx: &mut Context<Self>) {
        cx.emit(GitEvent::Notify(notification));
    }

    /// An error notification: `title` ("Checkout failed"), git's first line, and git's whole output
    /// behind "Details" when there is more to it.
    pub fn notify_error(&mut self, title: &str, error: &GitError, cx: &mut Context<Self>) {
        let mut notification = Notification::error(title.to_string()).body(error.to_string());
        if let Some(details) = error.details()
            && details
                .lines()
                .filter(|line| !line.trim().is_empty())
                .count()
                > 1
        {
            notification = notification.action(
                tr("Details"),
                ShowGitOutput {
                    title: title.to_string(),
                    output: details.to_string(),
                },
            );
        }
        self.notify(notification, cx);
    }

    // --- Repositories by name ---

    /// The folder name of a repository ("flux"): it tells repositories apart in lists.
    pub fn repo_name(&self, repo: usize) -> String {
        self.repos
            .get(repo)
            .and_then(|entry| entry.repo.work_dir.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// The repository the window works with now: the active file's, otherwise the one containing
    /// the project root, otherwise the first (as the title bar's branch).
    pub fn current_repo(&self, path: Option<&Path>) -> Option<usize> {
        path.and_then(|path| self.repo_index(path))
            .or_else(|| self.root.as_deref().and_then(|root| self.repo_index(root)))
            .or((!self.repos.is_empty()).then_some(0))
    }

    /// The current branch of a repository; `None` — detached HEAD (or not read yet).
    pub fn current_branch(&self, repo: usize) -> Option<String> {
        self.repos.get(repo)?.status.branch.head.clone()
    }

    // --- Branches, tags, stashes ---

    /// The branches and tags of a repository (empty until read).
    pub fn refs(&self, repo: usize) -> Arc<Refs> {
        self.repos
            .get(repo)
            .map(|entry| entry.refs.clone())
            .unwrap_or_default()
    }

    /// Re-reads the branches and tags (and the stashes, if they were asked for) in the background.
    pub fn reload_refs(&mut self, repo: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.repos.get_mut(repo) else {
            return;
        };
        if entry.stashes.is_some() {
            self.reload_stashes(repo, cx);
        }
        let entry = &mut self.repos[repo];
        if entry.refs_reading {
            entry.refs_stale = true;
            return;
        }
        entry.refs_reading = true;
        entry.refs_stale = false;
        let git_repo = entry.repo.clone();
        let read = cx.background_spawn(async move { flux_git::branch::refs(&git_repo) });
        cx.spawn(async move |this, cx| {
            let result = read.await;
            this.update(cx, |this, cx| {
                let Some(entry) = this.repos.get_mut(repo) else {
                    return;
                };
                entry.refs_reading = false;
                match result {
                    Ok(refs) => {
                        if *entry.refs != refs {
                            entry.refs = Arc::new(refs);
                            cx.notify();
                        }
                    }
                    Err(err) => log(|| format!("refs: {err}")),
                }
                if entry.refs_stale {
                    this.reload_refs(repo, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// The operation in progress in a repository.
    pub fn operation(&self, repo: usize) -> Arc<Operation> {
        self.repos
            .get(repo)
            .map(|entry| entry.operation.clone())
            .unwrap_or_default()
    }

    /// Repositories in the middle of a merge, rebase, cherry-pick or revert.
    pub fn repos_in_progress(&self) -> Vec<usize> {
        self.repos
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                !matches!(
                    entry.operation.state,
                    RepoState::Normal | RepoState::Bisecting
                )
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// The stashes of a repository; `None` — not read yet (the first call starts reading them, and
    /// from then on they follow refs changes).
    pub fn stashes(&mut self, repo: usize, cx: &mut Context<Self>) -> Option<Arc<Vec<Stash>>> {
        let entry = self.repos.get(repo)?;
        if entry.stashes.is_none() && !entry.stashes_reading {
            self.reload_stashes(repo, cx);
        }
        self.repos.get(repo)?.stashes.clone()
    }

    /// Re-reads the stashes of a repository in the background.
    pub fn reload_stashes(&mut self, repo: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.repos.get_mut(repo) else {
            return;
        };
        if entry.stashes_reading {
            return;
        }
        entry.stashes_reading = true;
        let git_repo = entry.repo.clone();
        let read = cx.background_spawn(async move { flux_git::stashes(&git_repo) });
        cx.spawn(async move |this, cx| {
            let result = read.await;
            this.update(cx, |this, cx| {
                let Some(entry) = this.repos.get_mut(repo) else {
                    return;
                };
                entry.stashes_reading = false;
                let stashes = result.unwrap_or_else(|err| {
                    log(|| format!("stashes: {err}"));
                    Vec::new()
                });
                if entry.stashes.as_deref() != Some(&stashes) {
                    entry.stashes = Some(Arc::new(stashes));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Whether a branch ("main", "origin/main") is a favorite of its repository.
    pub fn is_favorite(&self, repo: usize, name: &str) -> bool {
        self.repos
            .get(repo)
            .is_some_and(|entry| self.favorites.contains(&entry.repo.common_dir, name))
    }

    /// Stars or unstars a branch; the choice is saved.
    pub fn toggle_favorite(&mut self, repo: usize, name: &str, cx: &mut Context<Self>) {
        let Some(entry) = self.repos.get(repo) else {
            return;
        };
        let key = entry.repo.common_dir.clone();
        self.favorites.toggle(&key, name);
        cx.notify();
    }

    // --- Reading revisions ---

    /// The commit a revision names; `None` — no such commit.
    pub fn resolve_rev(
        &self,
        repo: usize,
        rev: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<Option<String>, GitError>> {
        self.read(repo, cx, {
            let rev = rev.to_string();
            move |repo| flux_git::branch::resolve(repo, &rev)
        })
    }

    /// Commits in `a` and not in `b`, and in `b` and not in `a` (Compare with Current).
    pub fn compare_commits(
        &self,
        repo: usize,
        a: &str,
        b: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<CommitsBothWays, GitError>> {
        let (a, b) = (a.to_string(), b.to_string());
        self.read(repo, cx, move |repo| {
            flux_git::compare_commits(repo, &a, &b, COMPARE_LIMIT)
        })
    }

    /// The files that differ between two revisions (`to: None` — the working tree), a renamed
    /// file with its old path (the left side of its diff).
    pub fn diff_changes(
        &self,
        repo: usize,
        from: &str,
        to: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Task<Result<Vec<FileChange>, GitError>> {
        let from = from.to_string();
        let to = to.map(str::to_string);
        self.read(repo, cx, move |repo| {
            flux_git::diff_changes(repo, &from, to.as_deref())
        })
    }

    /// The files of a stash with the old path of a renamed file.
    pub fn stash_changes(
        &self,
        repo: usize,
        oid: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<Vec<FileChange>, GitError>> {
        let oid = oid.to_string();
        self.read(repo, cx, move |repo| flux_git::stash_changes(repo, &oid))
    }

    /// The commits of `branch` that aren't in `into` (what deleting it would lose).
    pub fn unmerged_commits(
        &self,
        repo: usize,
        branch: &str,
        into: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<Vec<CommitInfo>, GitError>> {
        let (branch, into) = (branch.to_string(), into.to_string());
        self.read(repo, cx, move |repo| {
            flux_git::unmerged_commits(repo, &branch, &into, UNMERGED_LIMIT)
        })
    }

    /// The three versions of a conflicted file (relative path).
    pub fn conflict_versions(
        &self,
        repo: usize,
        relative: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<ConflictVersions, GitError>> {
        let Some(entry) = self.repos.get(repo) else {
            return Task::ready(Err(GitError::Canceled));
        };
        let blobs = entry.blobs.clone();
        let relative = relative.to_string();
        cx.background_spawn(async move { flux_git::conflict_versions(&blobs, &relative) })
    }

    /// The conflicted files of every repository.
    pub fn conflicts(&self) -> Vec<Change> {
        self.index
            .changes
            .iter()
            .filter(|change| change.status == FileStatus::Conflicted)
            .cloned()
            .collect()
    }

    /// What the two sides of a conflict are, for captions: (ours, theirs) — "main" and "feature/x"
    /// in a merge, the branch onto which a rebase goes and the commit it replays, the current branch
    /// and "Stash" after an unstash.
    pub fn conflict_sides(&self, repo: usize) -> (String, String) {
        let Some(entry) = self.repos.get(repo) else {
            return (tr("Yours").into(), tr("Theirs").into());
        };
        let op = &entry.operation;
        let refs = &entry.refs;
        let short = |oid: &str| oid.chars().take(7).collect::<String>();
        let name = |oid: &Option<String>| -> Option<String> {
            let oid = oid.as_deref()?;
            Some(
                refs.name_of(oid)
                    .map(str::to_string)
                    .unwrap_or_else(|| short(oid)),
            )
        };
        let current = entry
            .status
            .branch
            .label()
            .unwrap_or_else(|| tr("HEAD").to_string());
        match op.state {
            RepoState::Merging => (
                current,
                op.incoming_name
                    .clone()
                    .or_else(|| name(&op.incoming))
                    .unwrap_or_default(),
            ),
            RepoState::Rebasing => (
                name(&op.rebase_onto).unwrap_or_else(|| tr("Upstream").into()),
                op.stopped_at.as_deref().map(short).map_or_else(
                    || op.rebase_branch.clone().unwrap_or_default(),
                    |commit| match &op.rebase_branch {
                        Some(branch) => format!("{branch} · {commit}"),
                        None => commit,
                    },
                ),
            ),
            RepoState::CherryPicking | RepoState::Reverting => {
                (current, name(&op.incoming).unwrap_or_default())
            }
            RepoState::Normal | RepoState::Bisecting => (current, tr("Stash").into()),
        }
    }

    /// Runs a read-only git job of a repository in the background (an `impl GitStore` block of a
    /// flow's module may use it for a read of its own).
    pub(crate) fn read<T: Send + 'static>(
        &self,
        repo: usize,
        cx: &mut Context<Self>,
        job: impl FnOnce(&Repo) -> Result<T, GitError> + Send + 'static,
    ) -> Task<Result<T, GitError>> {
        let Some(entry) = self.repos.get(repo) else {
            return Task::ready(Err(GitError::Canceled));
        };
        let git_repo = entry.repo.clone();
        cx.background_spawn(async move { job(&git_repo) })
    }

    // --- Operations of 6.2: they return their result; the flows report it ---

    /// Runs an operation that changes a repository, in the background, one at a time per
    /// repository: `activity` is in the status bar meanwhile; after it the status, the refs and the
    /// open documents are brought up to date. (An `impl GitStore` block in a flow's module may add an
    /// operation through it.)
    pub(crate) fn run<T: Send + 'static>(
        &mut self,
        repo: usize,
        activity: &str,
        cx: &mut Context<Self>,
        job: impl FnOnce(&Repo) -> Result<T, GitError> + Send + 'static,
    ) -> Task<Result<T, GitError>> {
        self.run_with_progress(repo, activity, None, cx, move |repo, _| job(repo))
    }

    /// [`Self::run`] for a job that reports progress (fetch, pull): `progress` is the status bar
    /// text with a percent ("Fetching… {0}%").
    pub(crate) fn run_with_progress<T: Send + 'static>(
        &mut self,
        repo: usize,
        activity: &str,
        progress: Option<&'static str>,
        cx: &mut Context<Self>,
        job: impl FnOnce(&Repo, &(dyn Fn(PushProgress) + Sync)) -> Result<T, GitError> + Send + 'static,
    ) -> Task<Result<T, GitError>> {
        let Some(entry) = self.repos.get_mut(repo) else {
            return Task::ready(Err(GitError::Canceled));
        };
        if entry.busy {
            return Task::ready(Err(GitError::Failed {
                command: entry.repo.work_dir.display().to_string(),
                message: tr("Another Git operation is running in this repository").into(),
            }));
        }
        entry.busy = true;
        let git_repo = entry.repo.clone();
        self.set_activity(Some(activity.to_string().into()), cx);
        let (sender, mut steps) = mpsc::unbounded::<PushProgress>();
        let run = cx.background_spawn(async move {
            let report = move |step: PushProgress| {
                sender.unbounded_send(step).ok();
            };
            job(&git_repo, &report)
        });
        let activity = activity.to_string();
        let watch = cx.spawn(async move |this, cx| {
            while let Some(step) = steps.next().await {
                let (Some(template), Some(percent)) = (progress, step.percent) else {
                    continue;
                };
                let text = trf(template, &[&percent]);
                if this
                    .update(cx, |this, cx| this.set_activity(Some(text.into()), cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        cx.spawn(async move |this, cx| {
            let result = run.await;
            drop(watch);
            drop(activity);
            this.update(cx, |this, cx| {
                if let Some(entry) = this.repos.get_mut(repo) {
                    entry.busy = false;
                }
                this.set_activity(None, cx);
                this.refresh_repo(repo, cx);
                this.reload_refs(repo, cx);
                cx.emit(GitEvent::WorkTreeChanged(repo));
            })
            .ok();
            result
        })
    }

    /// Checks out a branch, a remote branch or a revision. `Smart`: local changes are stashed first
    /// and come back after (`unstash` in the result says how); `Force`: they are thrown away.
    pub fn checkout(
        &mut self,
        repo: usize,
        target: CheckoutTarget,
        mode: CheckoutMode,
        cx: &mut Context<Self>,
    ) -> Task<Result<CheckoutDone, GitError>> {
        self.run(repo, tr("Checking out…"), cx, move |repo| {
            let force = mode == CheckoutMode::Force;
            // The stash this checkout makes, popped exactly (not "the top one": another stash may
            // come meanwhile).
            let stashed = match mode {
                CheckoutMode::Smart => flux_git::stash::stash_push_oid(
                    repo,
                    &StashRequest {
                        message: format!(
                            "Flux: uncommitted changes before checkout of {}",
                            target.name()
                        ),
                        ..Default::default()
                    },
                )?,
                _ => None,
            };
            let done = match &target {
                CheckoutTarget::Local(name) | CheckoutTarget::Revision(name) => {
                    flux_git::branch::checkout(repo, name, force)
                }
                CheckoutTarget::Remote {
                    remote_branch,
                    local,
                } => flux_git::branch::checkout_remote(repo, remote_branch, local, force),
            };
            if let Err(err) = done {
                // The checkout didn't happen: the stashed changes go back where they were.
                if let Some(oid) = &stashed {
                    flux_git::stash::stash_apply(repo, oid, true, true)?;
                }
                return Err(err);
            }
            let unstash = match stashed {
                Some(oid) => Some((flux_git::stash::stash_apply(repo, &oid, true, false)?, oid)),
                None => None,
            };
            Ok(CheckoutDone { unstash })
        })
    }

    /// A new branch at `start`; `checkout` switches to it, `overwrite` resets an existing one.
    pub fn create_branch(
        &mut self,
        repo: usize,
        name: &str,
        start: &str,
        checkout: bool,
        overwrite: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), GitError>> {
        let (name, start) = (name.to_string(), start.to_string());
        self.run(repo, tr("Creating the branch…"), cx, move |repo| {
            flux_git::branch::create_branch(repo, &name, &start, checkout, overwrite)
        })
    }

    pub fn rename_branch(
        &mut self,
        repo: usize,
        old: &str,
        new: &str,
        unset_upstream: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), GitError>> {
        let (old, new) = (old.to_string(), new.to_string());
        self.run(repo, tr("Renaming the branch…"), cx, move |repo| {
            flux_git::branch::rename_branch(repo, &old, &new, unset_upstream)
        })
    }

    /// Deletes a local branch (`force` — even if it isn't merged).
    pub fn delete_branch(
        &mut self,
        repo: usize,
        name: &str,
        force: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<DeletedBranch, GitError>> {
        let name = name.to_string();
        self.run(repo, tr("Deleting the branch…"), cx, move |repo| {
            flux_git::branch::delete_branch(repo, &name, force)
        })
    }

    /// Brings a deleted branch back at its commit.
    pub fn restore_branch(
        &mut self,
        repo: usize,
        name: &str,
        oid: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), GitError>> {
        let (name, oid) = (name.to_string(), oid.to_string());
        self.run(repo, tr("Restoring the branch…"), cx, move |repo| {
            flux_git::branch::restore_branch(repo, &name, &oid)
        })
    }

    /// Deletes a branch on its remote ("origin/feature/x").
    pub fn delete_remote_branch(
        &mut self,
        repo: usize,
        remote_branch: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), GitError>> {
        let remote_branch = remote_branch.to_string();
        self.run_with_progress(
            repo,
            tr("Deleting the remote branch…"),
            None,
            cx,
            move |repo, progress| {
                let remotes = flux_git::remotes(repo)?;
                let (remote, branch) = remotes
                    .iter()
                    .find_map(|remote| {
                        let branch = remote_branch
                            .strip_prefix(remote.name.as_str())?
                            .strip_prefix('/')?;
                        Some((remote.name.clone(), branch.to_string()))
                    })
                    .ok_or_else(|| GitError::Failed {
                        command: "git push --delete".into(),
                        message: format!("No remote for {remote_branch}"),
                    })?;
                flux_git::branch::delete_remote_branch(
                    repo,
                    &remote,
                    &branch,
                    progress,
                    &flux_git::Cancel::new(),
                )
            },
        )
    }

    /// Brings a deleted tag back exactly as it was (`target` — what [`Self::delete_tag`] returned).
    pub fn restore_tag(
        &mut self,
        repo: usize,
        name: &str,
        target: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), GitError>> {
        let (name, target) = (name.to_string(), target.to_string());
        self.run(repo, tr("Restoring the tag…"), cx, move |repo| {
            flux_git::branch::restore_tag(repo, &name, &target)
        })
    }

    /// Deletes a tag; returns what it pointed to (the tag object of an annotated tag), for
    /// [`Self::restore_tag`].
    pub fn delete_tag(
        &mut self,
        repo: usize,
        name: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<String, GitError>> {
        let name = name.to_string();
        self.run(repo, tr("Deleting the tag…"), cx, move |repo| {
            flux_git::branch::delete_tag(repo, &name)
        })
    }

    /// Merges `rev` into the current branch; local changes are stashed around it.
    pub fn merge(
        &mut self,
        repo: usize,
        rev: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<Outcome, GitError>> {
        let rev = rev.to_string();
        self.run(repo, tr("Merging…"), cx, move |repo| {
            flux_git::branch::merge(repo, &rev, true)
        })
    }

    /// Rebases the current branch (or first checks out `branch`) onto `onto`; local changes are
    /// stashed around it.
    pub fn rebase(
        &mut self,
        repo: usize,
        onto: &str,
        branch: Option<String>,
        cx: &mut Context<Self>,
    ) -> Task<Result<Outcome, GitError>> {
        let onto = onto.to_string();
        self.run(repo, tr("Rebasing…"), cx, move |repo| {
            flux_git::branch::rebase(repo, &onto, branch.as_deref(), true)
        })
    }

    /// Fetches every remote of a repository.
    pub fn fetch(
        &mut self,
        repo: usize,
        cx: &mut Context<Self>,
    ) -> Task<Result<FetchResult, GitError>> {
        self.run_with_progress(
            repo,
            tr("Fetching…"),
            Some("Fetching… {0}%"),
            cx,
            |repo, progress| flux_git::sync::fetch(repo, None, progress, &flux_git::Cancel::new()),
        )
    }

    /// Pulls a remote branch into the current one.
    pub fn pull(
        &mut self,
        repo: usize,
        remote: &str,
        branch: &str,
        mode: PullMode,
        cx: &mut Context<Self>,
    ) -> Task<Result<Outcome, GitError>> {
        let (remote, branch) = (remote.to_string(), branch.to_string());
        self.run_with_progress(
            repo,
            tr("Pulling…"),
            Some("Pulling… {0}%"),
            cx,
            move |repo, progress| {
                flux_git::sync::pull(
                    repo,
                    &remote,
                    &branch,
                    mode,
                    progress,
                    &flux_git::Cancel::new(),
                )
            },
        )
    }

    /// Update Project for one repository: its current branch from its upstream.
    pub fn update(
        &mut self,
        repo: usize,
        method: UpdateMethod,
        cx: &mut Context<Self>,
    ) -> Task<Result<UpdateResult, GitError>> {
        self.run_with_progress(
            repo,
            tr("Updating…"),
            Some("Updating… {0}%"),
            cx,
            move |repo, progress| {
                flux_git::sync::update(repo, method, progress, &flux_git::Cancel::new())
            },
        )
    }

    /// Update of a branch that isn't checked out: a fast-forward from its upstream.
    pub fn fast_forward(
        &mut self,
        repo: usize,
        local: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<Outcome, GitError>> {
        let local = local.to_string();
        self.run_with_progress(
            repo,
            tr("Updating…"),
            Some("Updating… {0}%"),
            cx,
            move |repo, progress| {
                flux_git::sync::fast_forward(repo, &local, progress, &flux_git::Cancel::new())
            },
        )
    }

    /// Stashes local changes; `false` — there were none.
    pub fn stash(
        &mut self,
        repo: usize,
        request: StashRequest,
        cx: &mut Context<Self>,
    ) -> Task<Result<bool, GitError>> {
        self.run(repo, tr("Stashing…"), cx, move |repo| {
            flux_git::stash::stash_push(repo, &request)
        })
    }

    /// Applies (`pop` — and drops) a stash.
    pub fn unstash(
        &mut self,
        repo: usize,
        oid: &str,
        pop: bool,
        reinstate_index: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<Outcome, GitError>> {
        let oid = oid.to_string();
        self.run(repo, tr("Unstashing…"), cx, move |repo| {
            flux_git::stash::stash_apply(repo, &oid, pop, reinstate_index)
        })
    }

    /// A new branch from a stash (its base commit, the stash applied and dropped).
    pub fn stash_branch(
        &mut self,
        repo: usize,
        oid: &str,
        branch: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<Outcome, GitError>> {
        let (oid, branch) = (oid.to_string(), branch.to_string());
        self.run(repo, tr("Unstashing…"), cx, move |repo| {
            flux_git::stash::stash_branch(repo, &oid, &branch)
        })
    }

    pub fn drop_stash(
        &mut self,
        repo: usize,
        oid: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), GitError>> {
        let oid = oid.to_string();
        self.run(repo, tr("Dropping the stash…"), cx, move |repo| {
            flux_git::stash::stash_drop(repo, &oid)
        })
    }

    pub fn clear_stashes(
        &mut self,
        repo: usize,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), GitError>> {
        self.run(
            repo,
            tr("Clearing stashes…"),
            cx,
            flux_git::stash::stash_clear,
        )
    }

    /// Resolves conflicted files by taking one side whole.
    pub fn accept_side(
        &mut self,
        repo: usize,
        paths: Vec<String>,
        side: ConflictSide,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), GitError>> {
        self.run(repo, tr("Resolving…"), cx, move |repo| {
            flux_git::conflict::accept_side(repo, &paths, side)
        })
    }

    /// Marks a conflicted file resolved, writing the merge tool's result first.
    pub fn mark_resolved(
        &mut self,
        repo: usize,
        relative: &str,
        content: Option<Vec<u8>>,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), GitError>> {
        let relative = relative.to_string();
        self.run(repo, tr("Resolving…"), cx, move |repo| {
            flux_git::conflict::mark_resolved(repo, &relative, content.as_deref())
        })
    }

    /// Continues the operation in progress (rebase, cherry-pick, revert).
    pub fn continue_operation(
        &mut self,
        repo: usize,
        cx: &mut Context<Self>,
    ) -> Task<Result<Outcome, GitError>> {
        let state = self.operation(repo).state;
        self.run(repo, tr("Continuing…"), cx, move |repo| {
            flux_git::ops::continue_operation(repo, state)
        })
    }

    /// Aborts the operation in progress.
    pub fn abort_operation(
        &mut self,
        repo: usize,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), GitError>> {
        let state = self.operation(repo).state;
        self.run(repo, tr("Aborting…"), cx, move |repo| {
            flux_git::ops::abort_operation(repo, state)
        })
    }

    /// A rebase skips the commit it stopped on.
    pub fn skip_commit(
        &mut self,
        repo: usize,
        cx: &mut Context<Self>,
    ) -> Task<Result<Outcome, GitError>> {
        self.run(
            repo,
            tr("Skipping the commit…"),
            cx,
            flux_git::ops::skip_commit,
        )
    }
}

/// The commits of `a` that aren't in `b`, and of `b` that aren't in `a`.
pub type CommitsBothWays = (Vec<CommitInfo>, Vec<CommitInfo>);

/// How many commits Compare with Current lists each way.
const COMPARE_LIMIT: usize = 1000;
/// How many unmerged commits a delete question names.
const UNMERGED_LIMIT: usize = 20;

/// What to check out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckoutTarget {
    /// A local branch.
    Local(String),
    /// A remote branch ("origin/feature/x") as a new local branch `local` tracking it.
    Remote {
        remote_branch: String,
        local: String,
    },
    /// A tag or a commit: HEAD is detached.
    Revision(String),
}

impl CheckoutTarget {
    /// The name the user picked, for messages.
    pub fn name(&self) -> &str {
        match self {
            CheckoutTarget::Local(name) | CheckoutTarget::Revision(name) => name,
            CheckoutTarget::Remote { remote_branch, .. } => remote_branch,
        }
    }
}

/// What to do with local changes that are in the way of a checkout (JetBrains' question).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckoutMode {
    /// Plain checkout: changes that don't conflict come along, others make it fail.
    Normal,
    /// Smart Checkout: stash, checkout, unstash.
    Smart,
    /// Force Checkout: the local changes in the way are lost.
    Force,
}

/// A finished checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutDone {
    /// After a smart checkout: how the stashed changes came back (`Conflicts` — files to resolve,
    /// the stash is kept), and the stash's commit.
    pub unstash: Option<(Outcome, String)>,
}

/// Favorite branches per repository (by its common git directory), saved in
/// `~/Library/Application Support/flux/git-favorites.json` (`FLUX_GIT_FAVORITES_FILE` — another
/// file; in a scenario without it — in memory). A repository nobody starred anything in has
/// `main`, `master` and their `origin/` branches as favorites, as JetBrains IDEs do.
#[derive(Debug, Default)]
struct Favorites {
    by_repo: HashMap<String, Vec<String>>,
    path: Option<PathBuf>,
}

const DEFAULT_FAVORITES: [&str; 4] = ["main", "master", "origin/main", "origin/master"];

impl Favorites {
    fn load() -> Self {
        let explicit = std::env::var_os("FLUX_GIT_FAVORITES_FILE").filter(|file| !file.is_empty());
        let path = match explicit {
            Some(file) => Some(PathBuf::from(file)),
            None if std::env::var_os("FLUX_SCENARIO").is_some() => None,
            None => std::env::var_os("HOME").map(|home| {
                PathBuf::from(home).join("Library/Application Support/flux/git-favorites.json")
            }),
        };
        let by_repo = path
            .as_deref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .and_then(|value| value.as_object().cloned())
            .map(|repos| {
                repos
                    .into_iter()
                    .map(|(repo, names)| {
                        let names = names
                            .as_array()
                            .map(|names| {
                                names
                                    .iter()
                                    .filter_map(|name| name.as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_default();
                        (repo, names)
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self { by_repo, path }
    }

    fn contains(&self, repo: &Path, name: &str) -> bool {
        match self.by_repo.get(&repo.to_string_lossy().into_owned()) {
            Some(names) => names.iter().any(|favorite| favorite == name),
            None => DEFAULT_FAVORITES.contains(&name),
        }
    }

    fn toggle(&mut self, repo: &Path, name: &str) {
        let key = repo.to_string_lossy().into_owned();
        let names = self.by_repo.entry(key).or_insert_with(|| {
            DEFAULT_FAVORITES
                .iter()
                .map(|name| name.to_string())
                .collect()
        });
        match names.iter().position(|favorite| favorite == name) {
            Some(at) => {
                names.remove(at);
            }
            None => names.push(name.to_string()),
        }
        self.save();
    }

    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let value = serde_json::Value::Object(
            self.by_repo
                .iter()
                .map(|(repo, names)| (repo.clone(), serde_json::json!(names)))
                .collect(),
        );
        let text = serde_json::to_string_pretty(&value).unwrap_or_default() + "\n";
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        let temp = path.with_extension("json.flux-tmp");
        if std::fs::write(&temp, text).is_ok() && std::fs::rename(&temp, path).is_err() {
            std::fs::remove_file(&temp).ok();
        }
    }
}

/// `FLUX_GIT_LOG=1`: refresh timings and skipped events go to stderr.
fn log(message: impl FnOnce() -> String) {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    if *ENABLED.get_or_init(|| std::env::var_os("FLUX_GIT_LOG").is_some_and(|v| !v.is_empty())) {
        eprintln!("flux git: {}", message());
    }
}

/// The color of a file name for its change: modified blue, added green, untracked coral, deleted
/// gray, renamed teal, conflicted red (JetBrains' scheme).
pub fn status_color(status: FileStatus, ui: &UiColors) -> Hsla {
    match status {
        FileStatus::Modified | FileStatus::TypeChanged => ui.vcs_modified,
        FileStatus::Added => ui.vcs_added,
        FileStatus::Deleted => ui.vcs_deleted,
        FileStatus::Renamed => ui.vcs_renamed,
        FileStatus::Untracked => ui.vcs_untracked,
        FileStatus::Conflicted => ui.vcs_conflict,
    }
}

/// The color of a file's name in a tab or the tree; `None` — unchanged.
pub fn file_color(git: &Entity<GitStore>, path: &Path, ui: &UiColors, cx: &App) -> Option<Hsla> {
    git.read(cx)
        .status_of(path)
        .map(|status| status_color(status, ui))
}

/// The operation in progress, in the status bar: a running one ("Pushing… 45%"), otherwise the
/// state a repository is left in ("Merging · 2 conflicts", "Rebasing 2/5") — a click shows the
/// conflicts, or the commit window that concludes the operation.
pub fn status_item(git: &GitStore, ui: UiColors) -> Option<gpui::AnyElement> {
    if let Some(activity) = git.activity() {
        return Some(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap_1p5()
                .child(icon(IconName::Branch, ui.violet).size(px(13.)))
                .child(activity.clone())
                .into_any_element(),
        );
    }
    let conflicts = git.conflicts().len();
    let counted = crate::i18n::trn(conflicts, "{n} conflict", "{n} conflicts");
    let state = git.repos_in_progress().first().and_then(|&repo| {
        let operation = git.operation(repo);
        Some(match operation.state {
            RepoState::Merging => tr("Merging").to_string(),
            RepoState::Rebasing => match operation.step {
                Some((step, total)) => trf("Rebasing {0}/{1}", &[&step, &total]),
                None => tr("Rebasing").to_string(),
            },
            RepoState::CherryPicking => tr("Cherry-picking").to_string(),
            RepoState::Reverting => tr("Reverting").to_string(),
            RepoState::Normal | RepoState::Bisecting => return None,
        })
    });
    // Conflicts without an operation: an unstash that conflicted.
    let text = match (state, conflicts) {
        (Some(state), 0) => state,
        (Some(state), _) => format!("{state} · {counted}"),
        (None, 0) => return None,
        (None, _) => counted,
    };
    let color = if conflicts > 0 {
        ui.vcs_conflict
    } else {
        ui.warning
    };
    Some(
        div()
            .id("git-operation")
            .flex_none()
            .flex()
            .items_center()
            .gap_1p5()
            .px_1p5()
            .rounded(px(crate::ui::RADIUS_SM))
            .cursor_pointer()
            .text_color(color)
            .hover(move |style| style.bg(ui.hover))
            .on_click(move |_, window, cx| {
                if conflicts > 0 {
                    window.dispatch_action(Box::new(ResolveConflicts), cx)
                } else {
                    window.dispatch_action(Box::new(Commit), cx)
                }
            })
            .child(icon(IconName::Merge, color).size(px(13.)))
            .child(text)
            .into_any_element(),
    )
}

/// Window-level git actions: handled by the workspace (they open its windows).
pub fn workspace_actions(root: Div, cx: &mut Context<Workspace>) -> Div {
    root.on_action(
        cx.listener(|this, _: &Commit, window, cx| this.show_commit_window(true, window, cx)),
    )
    .on_action(
        cx.listener(|this, _: &ToggleCommitWindow, window, cx| {
            this.toggle_commit_window(window, cx)
        }),
    )
    .on_action(cx.listener(|this, _: &Push, window, cx| crate::push_dialog::open(this, window, cx)))
    .on_action(
        cx.listener(|this, _: &VcsOperations, window, cx| crate::vcs_menu::open(this, window, cx)),
    )
    .on_action(cx.listener(|this, _: &ShowDiff, window, cx| {
        if let Some(path) = this.active_path(cx) {
            this.open_diff(path, window, cx)
        }
    }))
    .on_action(
        cx.listener(|this, _: &Refresh, _, cx| this.git().update(cx, |git, cx| git.refresh(cx))),
    )
    .on_action(cx.listener(|_, action: &ShowGitOutput, window, cx| {
        // The answer doesn't matter: the dialog only shows git's output.
        drop(window.prompt(
            gpui::PromptLevel::Info,
            &action.title,
            Some(&action.output),
            &[tr("OK")],
            cx,
        ));
    }))
    .on_action(cx.listener(|this, action: &OpenCompareDiff, window, cx| {
        this.open_compare(
            action.path.clone(),
            action.repo,
            action.left.clone(),
            action.right.clone(),
            window,
            cx,
        )
    }))
}

/// The git branch of a directory, straight from `.git/HEAD` (before the first status): `ref:
/// refs/heads/main` → "main"; a detached HEAD gives the first 7 characters of the hash. A linked
/// worktree (`.git` is a file with `gitdir:`) is also read.
pub fn read_branch(root: &Path) -> Option<SharedString> {
    let git = root.join(".git");
    let dir = if git.is_file() {
        let link = std::fs::read_to_string(&git).ok()?;
        let target = link.strip_prefix("gitdir:")?.trim();
        root.join(target)
    } else {
        git
    };
    let head = std::fs::read_to_string(dir.join("HEAD")).ok()?;
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
    use flux_git::StatusEntry;

    fn hunk(old: std::ops::Range<u32>) -> Hunk {
        Hunk {
            new: old.clone(),
            old,
        }
    }

    #[test]
    fn checkboxes_follow_files_and_hunks() {
        let file = Path::new("/p/a.rs");
        let hunks = [hunk(1..2), hunk(5..6), hunk(9..9)];
        let mut inclusion = Inclusion::default();
        // Tracked changes are in by default, untracked files are out.
        assert_eq!(inclusion.state(file, true), CheckState::Checked);
        assert_eq!(inclusion.state(file, false), CheckState::Unchecked);
        // One hunk out: partial; all out: unchecked; one back in: partial again.
        inclusion.set_hunk(file, &hunks[1], false, &hunks, true);
        assert_eq!(inclusion.state(file, true), CheckState::Partial);
        assert!(!inclusion.hunk_included(file, &hunks[1], true));
        assert!(inclusion.hunk_included(file, &hunks[0], true));
        inclusion.set_hunk(file, &hunks[0], false, &hunks, true);
        inclusion.set_hunk(file, &hunks[2], false, &hunks, true);
        assert_eq!(inclusion.state(file, true), CheckState::Unchecked);
        inclusion.set_hunk(file, &hunks[2], true, &hunks, true);
        assert_eq!(inclusion.state(file, true), CheckState::Partial);
        assert!(inclusion.hunk_included(file, &hunks[2], true));
        assert!(!inclusion.hunk_included(file, &hunks[0], true));
        // The last excluded hunk back in: the whole file.
        inclusion.set_hunk(file, &hunks[0], true, &hunks, true);
        inclusion.set_hunk(file, &hunks[1], true, &hunks, true);
        assert_eq!(inclusion.state(file, true), CheckState::Checked);
        // The file's checkbox drops the hunk choices.
        inclusion.set_hunk(file, &hunks[0], false, &hunks, true);
        inclusion.set_file(file, true);
        assert_eq!(inclusion.state(file, true), CheckState::Checked);
        // An untracked file gets in with one hunk checked.
        let new = Path::new("/p/new.rs");
        let only = [hunk(0..0)];
        inclusion.set_hunk(new, &only[0], true, &only, false);
        assert_eq!(inclusion.state(new, false), CheckState::Checked);
        // HEAD moved: hunk keys are forgotten, file choices stay.
        inclusion.set_hunk(file, &hunks[0], false, &hunks, true);
        inclusion.forget_hunks_under(Path::new("/p"));
        assert_eq!(inclusion.state(file, true), CheckState::Checked);
        inclusion.retain(|path| path != new);
        assert_eq!(inclusion.state(new, false), CheckState::Unchecked);
    }

    #[test]
    fn index_colors_directories() {
        let repo = Repo {
            work_dir: PathBuf::from("/p"),
            git_dir: PathBuf::from("/p/.git"),
            common_dir: PathBuf::from("/p/.git"),
        };
        let entry = |path: &str, status| StatusEntry {
            path: path.into(),
            orig_path: None,
            status,
            staged: false,
            unstaged: true,
            conflict: None,
        };
        let status = RepoStatus {
            entries: vec![
                entry("src/app/main.rs", FileStatus::Modified),
                entry("notes/todo.md", FileStatus::Untracked),
                entry("src/new.rs", FileStatus::Untracked),
            ],
            ..Default::default()
        };
        let untracked = [PathBuf::from("/p/notes")];
        let index = StatusIndex::build(
            Some(Path::new("/p")),
            [(&repo, &status, &untracked[..])].into_iter(),
        );
        assert_eq!(index.changes.len(), 3);
        assert_eq!(
            index.statuses.get(Path::new("/p/src/new.rs")),
            Some(&FileStatus::Untracked)
        );
        // A tracked change colors its directories up to the root; untracked files don't.
        assert_eq!(
            index.dir_status(Path::new("/p/src")),
            Some(FileStatus::Modified)
        );
        assert_eq!(
            index.dir_status(Path::new("/p/src/app")),
            Some(FileStatus::Modified)
        );
        assert!(index.changed_dirs.contains(Path::new("/p")));
        assert!(!index.changed_dirs.contains(Path::new("/")));
        // A wholly untracked directory, and what's inside it.
        assert_eq!(
            index.dir_status(Path::new("/p/notes")),
            Some(FileStatus::Untracked)
        );
        assert_eq!(
            index.dir_status(Path::new("/p/notes/sub")),
            Some(FileStatus::Untracked)
        );
        assert_eq!(index.dir_status(Path::new("/p/docs")), None);
    }

    #[test]
    fn branch_comes_from_head() {
        let branch = |head: &str| branch_from_head(head).map(|b| b.to_string());
        assert_eq!(branch("ref: refs/heads/main\n").as_deref(), Some("main"));
        assert_eq!(
            branch("ref: refs/heads/feature/x").as_deref(),
            Some("feature/x")
        );
        assert_eq!(branch("4d7de13a9f00c0ffee\n").as_deref(), Some("4d7de13"));
        assert_eq!(branch(""), None);
    }
}
