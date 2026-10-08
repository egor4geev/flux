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
    BlobReader, CommitRequest, CommitResult, FileStatus, GitError, Hunk, PushProgress, PushRequest,
    PushResult, Repo, RepoEvent, RepoStatus, RepoWatcher, RollbackFile,
};
use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    App, AppContext, Context, Div, Entity, EventEmitter, Hsla, InteractiveElement, IntoElement,
    KeyBinding, NoAction, ParentElement, SharedString, Styled, Task, WeakEntity, actions, div, px,
};

use crate::editor::Editor;
use crate::i18n::{tr, trf};
use crate::icons::{IconName, icon};
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
    ]
);

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
        // Programs in a terminal need ⌃V (vim's visual block, a literal next character).
        KeyBinding::new("ctrl-v", NoAction, Some("Terminal")),
    ]);
}

/// What changed, for those who watch the store.
#[derive(Debug, Clone, PartialEq)]
pub enum GitEvent {
    /// A message for the status bar ("Committed 3 files").
    Message(SharedString),
    /// An operation failed: a short message and git's whole output (hook output, a rejected push)
    /// for a dialog.
    Error {
        message: SharedString,
        details: Option<String>,
    },
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
            _watcher: watcher,
            _watch_task: watch_task,
        });
        self.read_ignored(index, cx);
        self.refresh_repo(index, cx);
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
        let mut skipped = 0;
        for event in &batch {
            match event {
                RepoEvent::Git | RepoEvent::Rescan => refresh = true,
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
            Ok::<_, GitError>((status, dirs, started.elapsed()))
        });
        cx.spawn(async move |this, cx| {
            let result = read.await;
            this.update(cx, |this, cx| {
                let Some(entry) = this.repos.get_mut(index) else {
                    return;
                };
                entry.refreshing = false;
                let (changed, head_moved) = match result {
                    Ok((status, dirs, took)) => {
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
                        let changed = *entry.status != status || entry.untracked_dirs != dirs;
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

/// The operation in progress, in the status bar.
pub fn status_item(git: &GitStore, ui: UiColors) -> Option<impl IntoElement + use<>> {
    let activity = git.activity()?.clone();
    Some(
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_1p5()
            .child(icon(IconName::Branch, ui.violet).size(px(13.)))
            .child(activity),
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
