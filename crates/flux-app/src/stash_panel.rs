//! Stashes, as the Stash tab of the Commit tool window in JetBrains IDEs: the commit window's
//! second tab lists the stashes of every repository (message, branch, age), a stash opens to its
//! files, a double click on a file shows its diff (the stash against the commit it was made on);
//! Apply, Pop, Unstash as Branch…, Drop, Clear; and the Stash Changes… dialog (a message, untracked
//! files, keep index — or only the files selected in the commit window). An unstash that conflicts
//! leaves the stash and opens the Conflicts dialog.
//!
//! - The keyboard: ↑↓, ←→ (a stash's files), ↵ (the diff of a file; on a stash, its files), ⌫
//!   (Drop, after a question), ⇧F10 (the context menu), Esc (back to the editor).
//! - Stashes are read only while the tab is shown (`GitStore::stashes`), then they follow the refs.
//!   A stash's files are read when it is opened and kept by its commit (a stash never changes).
//! - The flows that need the window (the dialogs) go through actions the workspace handles
//!   ([`workspace_actions`]); questions and git operations run right here.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use flux_git::{FileChange, FileStatus, GitError, Outcome, Stash, StashRequest};
use gpui::{
    Action, AnyElement, App, AsyncWindowContext, ClickEvent, Context, DismissEvent, Div, Entity,
    EventEmitter, FocusHandle, Focusable, FontWeight, KeyBinding, MouseButton, MouseDownEvent,
    Pixels, Point, Render, ScrollStrategy, SharedString, Subscription, UniformListScrollHandle,
    Window, actions, div, prelude::*, px, uniform_list,
};

use crate::context_menu::ContextMenu;
use crate::dialog::Dialog;
use crate::diff_view::DiffSide;
use crate::git::{self, GitStore};
use crate::i18n::{tr, trf, trn};
use crate::icons::{IconName, file_icon, icon};
use crate::input::{InputEvent, TextInput};
use crate::input_dialog::InputDialog;
use crate::notifications::Notification;
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, CheckState, RADIUS_SM};
use crate::workspace::Workspace;

const ROW_HEIGHT: f32 = 26.;
/// Rows are inset from the island's edges; the highlight is a rounded box inside the row.
const ROW_INSET: f32 = 6.;
const ROW_PADDING: f32 = 6.;
const INDENT: f32 = 14.;
const CHEVRON_WIDTH: f32 = 16.;
const CHEVRON_SIZE: f32 = 12.;
/// How many files a "Stash Selected Files" dialog names before "and N more".
const LISTED_FILES: usize = 3;
const DIALOG_WIDTH: f32 = 440.;
const ROW_GROUP: &str = "stash-row";

// The list (context "StashList").
actions!(
    stash_panel,
    [
        SelectNext,
        SelectPrevious,
        SelectFirst,
        SelectLast,
        Expand,
        Collapse,
        /// ↵: the diff of the selected file; on a stash, its files.
        Open,
        /// Applies the selected stash and keeps it.
        Apply,
        /// Applies the selected stash and drops it.
        Pop,
        /// A new branch from the selected stash.
        UnstashAsBranch,
        /// Drops the selected stash, after a question.
        Drop,
        /// Drops every stash of the selected stash's repository, after a question.
        Clear,
        Refresh,
        ShowContextMenu,
        Cancel,
    ]
);

// The Stash Changes dialog (context "StashDialog"): ↑↓ choose the repository.
actions!(stash_dialog, [Confirm, Dismiss, NextRepo, PreviousRepo]);

/// Stash Selected Files… from the commit window: the dialog for these paths of a repository.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = stash_panel, no_json)]
pub struct StashSelected {
    pub repo: usize,
    /// Relative to the working tree, `/`.
    pub paths: Vec<String>,
    /// Some of them are untracked: the stash takes untracked files too.
    pub untracked: bool,
}

/// Unstash as Branch…: the dialog for the new branch's name.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = stash_panel, no_json)]
pub struct UnstashToBranch {
    pub repo: usize,
    pub oid: String,
}

pub fn init(cx: &mut App) {
    let list = Some("StashList");
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, list),
        KeyBinding::new("up", SelectPrevious, list),
        KeyBinding::new("home", SelectFirst, list),
        KeyBinding::new("end", SelectLast, list),
        KeyBinding::new("right", Expand, list),
        KeyBinding::new("left", Collapse, list),
        KeyBinding::new("enter", Open, list),
        // As Delete in the Stash tab of JetBrains IDEs.
        KeyBinding::new("backspace", Drop, list),
        KeyBinding::new("delete", Drop, list),
        KeyBinding::new("cmd-backspace", Drop, list),
        KeyBinding::new("shift-f10", ShowContextMenu, list),
        KeyBinding::new("escape", Cancel, list),
    ]);
    let dialog = Some("StashDialog");
    cx.bind_keys([
        KeyBinding::new("enter", Confirm, dialog),
        KeyBinding::new("escape", Dismiss, dialog),
        KeyBinding::new("down", NextRepo, dialog),
        KeyBinding::new("up", PreviousRepo, dialog),
    ]);
}

/// What the Stash tab asks the commit window (and the workspace) to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StashPanelEvent {
    /// Esc: back to the editor.
    FocusEditor,
}

// --- Rows ---

/// What a row is, kept across refreshes: the selection follows it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum StashKey {
    Repo(usize),
    /// A stash, by its repository and commit.
    Stash(usize, String),
    /// A file of a stash: the repository, the stash's commit, the path (relative).
    File(usize, String, String),
    /// A line in place of a stash's files: reading, none, an error.
    Note(usize, String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StashRowKind {
    Repo { name: String, count: usize },
    Stash(Stash),
    File(FileChange),
    Note(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StashRow {
    pub key: StashKey,
    pub depth: usize,
    pub kind: StashRowKind,
    pub expanded: bool,
}

impl StashRow {
    fn expandable(&self) -> bool {
        matches!(
            self.kind,
            StashRowKind::Repo { .. } | StashRowKind::Stash(_)
        )
    }

    /// The repository of the row.
    fn repo(&self) -> usize {
        match &self.key {
            StashKey::Repo(repo)
            | StashKey::Stash(repo, _)
            | StashKey::File(repo, _, _)
            | StashKey::Note(repo, _) => *repo,
        }
    }
}

/// The files of a stash, once asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StashFiles {
    Loading,
    Loaded(Vec<FileChange>),
    Failed(String),
}

/// The stashes of one repository, for [`build_rows`].
pub(crate) struct RepoStashes<'a> {
    pub repo: usize,
    pub name: String,
    pub stashes: &'a [Stash],
}

/// The visible rows: with several repositories, a node per repository that has stashes; its
/// stashes, newest first; under an open stash, its files by path (or a note while they are read).
pub(crate) fn build_rows(
    repos: &[RepoStashes],
    several: bool,
    expanded: &HashSet<(usize, String)>,
    collapsed: &HashSet<usize>,
    files: &HashMap<(usize, String), StashFiles>,
) -> Vec<StashRow> {
    let mut rows = Vec::new();
    for entry in repos {
        if entry.stashes.is_empty() {
            continue;
        }
        let mut depth = 0;
        if several {
            let open = !collapsed.contains(&entry.repo);
            rows.push(StashRow {
                key: StashKey::Repo(entry.repo),
                depth,
                kind: StashRowKind::Repo {
                    name: entry.name.clone(),
                    count: entry.stashes.len(),
                },
                expanded: open,
            });
            if !open {
                continue;
            }
            depth = 1;
        }
        for stash in entry.stashes {
            let id = (entry.repo, stash.oid.clone());
            let open = expanded.contains(&id);
            rows.push(StashRow {
                key: StashKey::Stash(entry.repo, stash.oid.clone()),
                depth,
                kind: StashRowKind::Stash(stash.clone()),
                expanded: open,
            });
            if !open {
                continue;
            }
            let note = |text: &str| StashRow {
                key: StashKey::Note(entry.repo, stash.oid.clone()),
                depth: depth + 1,
                kind: StashRowKind::Note(text.to_string()),
                expanded: false,
            };
            match files.get(&id) {
                None | Some(StashFiles::Loading) => rows.push(note(tr("Reading…"))),
                Some(StashFiles::Failed(error)) => rows.push(note(error)),
                Some(StashFiles::Loaded(list)) if list.is_empty() => {
                    rows.push(note(tr("No files")))
                }
                Some(StashFiles::Loaded(list)) => {
                    let mut list = list.clone();
                    list.sort_by(|a, b| compare_paths(&a.path, &b.path));
                    for file in list {
                        rows.push(StashRow {
                            key: StashKey::File(entry.repo, stash.oid.clone(), file.path.clone()),
                            depth: depth + 1,
                            kind: StashRowKind::File(file),
                            expanded: false,
                        });
                    }
                }
            }
        }
    }
    rows
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

/// `stash@{2}`.
pub(crate) fn stash_name(stash: &Stash) -> String {
    format!("stash@{{{}}}", stash.index)
}

/// "stash@{1}: Parser experiment". Built without a template: the name's braces would read as one
/// of its placeholders.
pub(crate) fn stash_label(stash: &Stash) -> String {
    format!("{}: {}", stash_name(stash), stash_title(stash))
}

/// "stash@{1} · on main".
fn stash_place(stash: &Stash) -> String {
    match &stash.branch {
        Some(branch) => format!("{} · {}", stash_name(stash), trf("on {0}", &[branch])),
        None => stash_name(stash),
    }
}

/// The message a row shows: the user's own, or git's "WIP on main: …"; never empty.
pub(crate) fn stash_title(stash: &Stash) -> String {
    let message = stash.message.trim();
    if message.is_empty() {
        stash_name(stash)
    } else {
        message.to_string()
    }
}

/// The two sides of a stashed file's diff: the commit the stash was made on (a renamed file — at
/// its old path), and the stash (an untracked file is in the stash's third parent).
pub(crate) fn diff_sides(stash: &Stash, file: &FileChange) -> (DiffSide, DiffSide) {
    let untracked = file.status == FileStatus::Untracked;
    let left = DiffSide::Revision {
        rev: flux_git::stash::stash_base(&stash.oid),
        path: file.orig_path.clone().unwrap_or_else(|| file.path.clone()),
        label: tr("Before the stash").to_string(),
    };
    let right = DiffSide::Revision {
        rev: if untracked {
            flux_git::stash::untracked_commit(&stash.oid)
        } else {
            stash.oid.clone()
        },
        path: file.path.clone(),
        label: trf("Stash · {0}", &[&stash_title(stash)]),
    };
    (left, right)
}

/// A popup menu of the tab.
struct Menu {
    menu: Entity<ContextMenu>,
    position: Point<Pixels>,
    _subscriptions: [Subscription; 2],
}

/// The Stash tab of the commit window.
pub struct StashPanel {
    git: Entity<GitStore>,
    focus_handle: FocusHandle,
    scroll: UniformListScrollHandle,
    /// The tab is shown: stashes are read only then.
    active: bool,
    /// Repositories (index, folder name) and their stashes as last read; `loading` — some aren't
    /// read yet.
    lists: Vec<(usize, String, Arc<Vec<Stash>>)>,
    loading: bool,
    rows: Vec<StashRow>,
    selected: Option<StashKey>,
    /// Open stashes, by repository and commit; collapsed repository nodes.
    expanded: HashSet<(usize, String)>,
    collapsed: HashSet<usize>,
    files: HashMap<(usize, String), StashFiles>,
    menu: Option<Menu>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<StashPanelEvent> for StashPanel {}

impl StashPanel {
    pub fn new(git: Entity<GitStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let _ = window;
        let subscriptions = vec![cx.observe(&git, |this, _, cx| this.reload(cx))];
        Self {
            git,
            focus_handle: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            active: false,
            lists: Vec::new(),
            loading: false,
            rows: Vec::new(),
            selected: None,
            expanded: HashSet::new(),
            collapsed: HashSet::new(),
            files: HashMap::new(),
            menu: None,
            _subscriptions: subscriptions,
        }
    }

    /// The tab is shown (its stashes are read now and follow the refs) or hidden.
    pub fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.active == active {
            return;
        }
        self.active = active;
        if active {
            self.reload(cx);
        }
    }

    /// Focus in the list; the first row is selected if nothing is.
    pub fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle);
        if self.selected_index().is_none() && !self.rows.is_empty() {
            self.select_row(0, ScrollStrategy::Top, cx);
        }
    }

    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus_handle.contains_focused(window, cx)
            || self
                .menu
                .as_ref()
                .is_some_and(|menu| menu.menu.focus_handle(cx).contains_focused(window, cx))
    }

    // --- Reading ---

    /// Takes the stashes from the hub (the first call for a repository starts reading them).
    fn reload(&mut self, cx: &mut Context<Self>) {
        if !self.active {
            return;
        }
        let count = self.git.read(cx).repos().len();
        type Read = (usize, String, Option<Arc<Vec<Stash>>>);
        let lists: Vec<Read> = self.git.update(cx, |git, cx| {
            (0..count)
                .map(|repo| (repo, git.repo_name(repo), git.stashes(repo, cx)))
                .collect()
        });
        self.loading = lists.iter().any(|(_, _, stashes)| stashes.is_none());
        self.lists = lists
            .into_iter()
            .map(|(repo, name, stashes)| (repo, name, stashes.unwrap_or_default()))
            .collect();
        // What is gone is forgotten; an open stash gets its files.
        let alive: HashSet<(usize, String)> = self
            .lists
            .iter()
            .flat_map(|(repo, _, stashes)| {
                stashes.iter().map(move |stash| (*repo, stash.oid.clone()))
            })
            .collect();
        self.expanded.retain(|id| alive.contains(id));
        self.files.retain(|id, _| alive.contains(id));
        for id in self.expanded.clone() {
            self.load_files(id, cx);
        }
        self.rebuild(cx);
    }

    fn load_files(&mut self, id: (usize, String), cx: &mut Context<Self>) {
        if self.files.contains_key(&id) {
            return;
        }
        self.files.insert(id.clone(), StashFiles::Loading);
        let (repo, oid) = id.clone();
        let read = self
            .git
            .update(cx, |git, cx| git.stash_changes(repo, &oid, cx));
        cx.spawn(async move |this, cx| {
            let result = read.await;
            this.update(cx, |this, cx| {
                let files = match result {
                    Ok(files) => StashFiles::Loaded(files),
                    Err(err) => StashFiles::Failed(err.to_string()),
                };
                this.files.insert(id, files);
                this.rebuild(cx);
            })
            .ok();
        })
        .detach();
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let was_empty = self.rows.is_empty();
        let repos: Vec<RepoStashes> = self
            .lists
            .iter()
            .map(|(repo, name, stashes)| RepoStashes {
                repo: *repo,
                name: name.clone(),
                stashes,
            })
            .collect();
        let several = self.lists.len() > 1;
        self.rows = build_rows(
            &repos,
            several,
            &self.expanded,
            &self.collapsed,
            &self.files,
        );
        // A selected row that went away: its stash, else its repository's first row.
        if self.selected_index().is_none() {
            self.selected = match self.selected.take() {
                Some(StashKey::File(repo, oid, _) | StashKey::Note(repo, oid)) => {
                    let key = StashKey::Stash(repo, oid);
                    self.rows
                        .iter()
                        .any(|row| row.key == key)
                        .then_some(key)
                        .or_else(|| self.first_row_of(repo))
                }
                Some(StashKey::Stash(repo, _) | StashKey::Repo(repo)) => self.first_row_of(repo),
                // The list has just come: its newest stash is selected, Apply and Pop are ready.
                None if was_empty => self
                    .rows
                    .iter()
                    .find(|row| matches!(row.kind, StashRowKind::Stash(_)))
                    .map(|row| row.key.clone()),
                None => None,
            };
        }
        cx.notify();
    }

    fn first_row_of(&self, repo: usize) -> Option<StashKey> {
        self.rows
            .iter()
            .find(|row| row.repo() == repo && matches!(row.kind, StashRowKind::Stash(_)))
            .map(|row| row.key.clone())
    }

    /// Re-reads every repository's stashes.
    fn refresh(&mut self, _: &Refresh, _: &mut Window, cx: &mut Context<Self>) {
        let count = self.git.read(cx).repos().len();
        self.git.update(cx, |git, cx| {
            for repo in 0..count {
                git.reload_stashes(repo, cx);
            }
        });
    }

    // --- Selection ---

    fn selected_index(&self) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        self.rows.iter().position(|row| row.key == *selected)
    }

    fn selected_row(&self) -> Option<&StashRow> {
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

    /// The stash of the selected row (a stash or one of its files) and its repository.
    fn selected_stash(&self) -> Option<(usize, Stash)> {
        let (repo, oid) = match self.selected.as_ref()? {
            StashKey::Stash(repo, oid)
            | StashKey::File(repo, oid, _)
            | StashKey::Note(repo, oid) => (*repo, oid),
            StashKey::Repo(_) => return None,
        };
        self.find_stash(repo, oid).map(|stash| (repo, stash))
    }

    fn find_stash(&self, repo: usize, oid: &str) -> Option<Stash> {
        self.lists
            .iter()
            .find(|(index, _, _)| *index == repo)?
            .2
            .iter()
            .find(|stash| stash.oid == oid)
            .cloned()
    }

    /// The repository the selection is in; with nothing selected, the first one with stashes.
    fn selected_repo(&self) -> Option<usize> {
        self.selected_row().map(StashRow::repo).or_else(|| {
            self.lists
                .iter()
                .find(|(_, _, stashes)| !stashes.is_empty())
                .map(|(repo, _, _)| *repo)
        })
    }

    fn set_expanded(&mut self, key: &StashKey, open: bool, cx: &mut Context<Self>) {
        match key {
            StashKey::Repo(repo) => {
                if open {
                    self.collapsed.remove(repo);
                } else {
                    self.collapsed.insert(*repo);
                }
            }
            StashKey::Stash(repo, oid) => {
                let id = (*repo, oid.clone());
                if open {
                    self.expanded.insert(id.clone());
                    self.load_files(id, cx);
                } else {
                    self.expanded.remove(&id);
                }
            }
            _ => return,
        }
        self.rebuild(cx);
    }

    /// → : opens a stash (its files) or a repository; on an open one, goes to its first child.
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

    /// ← : closes an open node; otherwise goes to the parent.
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

    // --- Operations ---

    /// ↵: the diff of a file; on a stash or a repository, opens or closes it.
    fn open(&mut self, _: &Open, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.selected_row().cloned() else {
            return;
        };
        match &row.key {
            StashKey::File(repo, oid, path) => self.show_diff(*repo, oid, path, window, cx),
            _ if row.expandable() => self.set_expanded(&row.key, !row.expanded, cx),
            _ => {}
        }
    }

    /// The diff of a stashed file: the commit the stash was made on ↔ the stash.
    fn show_diff(
        &mut self,
        repo: usize,
        oid: &str,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(stash) = self.find_stash(repo, oid) else {
            return;
        };
        let Some(StashFiles::Loaded(files)) = self.files.get(&(repo, oid.to_string())) else {
            return;
        };
        let Some(file) = files.iter().find(|file| file.path == path).cloned() else {
            return;
        };
        let Some(work_dir) = self
            .git
            .read(cx)
            .repos()
            .get(repo)
            .map(|entry| entry.repo.work_dir.clone())
        else {
            return;
        };
        let (left, right) = diff_sides(&stash, &file);
        let action = git::OpenCompareDiff {
            repo,
            path: work_dir.join(path),
            left,
            right,
        };
        window.dispatch_action(Box::new(action), cx);
    }

    fn apply(&mut self, pop: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some((repo, stash)) = self.selected_stash() else {
            return;
        };
        unstash(self.git.clone(), repo, stash, pop, window, cx);
    }

    fn unstash_as_branch(
        &mut self,
        _: &UnstashAsBranch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((repo, stash)) = self.selected_stash() else {
            return;
        };
        let action = UnstashToBranch {
            repo,
            oid: stash.oid,
        };
        window.dispatch_action(Box::new(action), cx);
    }

    fn drop_stash(&mut self, _: &Drop, window: &mut Window, cx: &mut Context<Self>) {
        let Some((repo, stash)) = self.selected_stash() else {
            return;
        };
        confirm_drop(self.git.clone(), repo, stash, window, cx);
    }

    fn clear(&mut self, _: &Clear, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repo) = self.selected_repo() else {
            return;
        };
        let count = self
            .lists
            .iter()
            .find(|(index, _, _)| *index == repo)
            .map_or(0, |(_, _, stashes)| stashes.len());
        if count == 0 {
            return;
        }
        let several = self.lists.len() > 1;
        let name = self.git.read(cx).repo_name(repo);
        let question = if several {
            trf("Clear all stashes of {0}?", &[&name])
        } else {
            tr("Clear all stashes?").to_string()
        };
        let detail = trn(
            count,
            "{n} stash will be deleted. This can't be undone.",
            "{n} stashes will be deleted. This can't be undone.",
        );
        let answer = Dialog::warning(question)
            .message(detail)
            .danger(tr("Clear"))
            .cancel(tr("Cancel"))
            .show(window, cx);
        let git = self.git.clone();
        cx.spawn(async move |_, cx| {
            if answer.await != Some(0) {
                return;
            }
            let Ok(task) = git.update(cx, |git, cx| git.clear_stashes(repo, cx)) else {
                return;
            };
            let result = task.await;
            git.update(cx, |git, cx| match result {
                Ok(()) => git.notify(
                    Notification::success(trn(count, "Cleared {n} stash", "Cleared {n} stashes")),
                    cx,
                ),
                Err(err) => git.notify_error(tr("Couldn't clear the stashes"), &err, cx),
            })
            .ok();
        })
        .detach();
    }

    // --- Mouse and menus ---

    fn click_row(
        &mut self,
        key: StashKey,
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
                if let StashKey::File(repo, oid, path) = &key {
                    self.show_diff(*repo, oid, path, window, cx);
                }
            }
            _ => {}
        }
        cx.notify();
    }

    fn secondary_click(
        &mut self,
        key: Option<StashKey>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        if key.is_some() {
            self.selected = key;
        }
        let row = self.selected_row().cloned();
        let has_stash = self.selected_stash().is_some();
        let file = matches!(row.as_ref().map(|row| &row.key), Some(StashKey::File(..)));
        let stashes = !self.rows.is_empty();
        let menu = cx.new(|cx| {
            ContextMenu::new(window, cx)
                .entry_if(file, tr("Show Diff"), Open)
                .separator()
                .entry_if(has_stash, tr("Apply"), Apply)
                .entry_if(has_stash, tr("Pop"), Pop)
                .entry_if(has_stash, tr("Unstash as Branch…"), UnstashAsBranch)
                .separator()
                .entry_if(has_stash, tr("Drop…"), Drop)
                .entry_if(stashes, tr("Clear…"), Clear)
                .separator()
                .entry(tr("Stash Changes…"), git::StashChanges)
                .entry(tr("Refresh"), Refresh)
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

    // --- Rendering ---

    /// The tab's toolbar (the commit window shows it under its tabs): its buttons act on the list.
    pub fn toolbar(&self, window: &Window, cx: &App) -> Div {
        let ui = Theme::ui(cx);
        let has_stash = self.selected_stash().is_some();
        let any = !self.rows.is_empty();
        let focus = self.focus_handle.clone();
        let button = |id: &'static str,
                      name: IconName,
                      label: &'static str,
                      action: Box<dyn Action>,
                      enabled: bool| {
            let keys = ui::shortcut_in(action.as_ref(), &focus, window);
            let focus = focus.clone();
            ui::icon_button(id, name, ui)
                .tooltip(ui::tooltip(label, keys))
                .when(!enabled, |button| button.opacity(0.4))
                .on_click(move |_, window, cx| {
                    if enabled {
                        window.focus(&focus);
                        window.dispatch_action(action.boxed_clone(), cx);
                    }
                })
        };
        div()
            .flex()
            .items_center()
            .gap_0p5()
            .child(button(
                "stash-new",
                IconName::Stash,
                tr("Stash Changes…"),
                Box::new(git::StashChanges),
                true,
            ))
            .child(button(
                "stash-refresh",
                IconName::Refresh,
                tr("Refresh"),
                Box::new(Refresh),
                true,
            ))
            .child(div().w_1())
            .child(button(
                "stash-branch",
                IconName::Branch,
                tr("Unstash as Branch…"),
                Box::new(UnstashAsBranch),
                has_stash,
            ))
            .child(button(
                "stash-drop",
                IconName::Minus,
                tr("Drop…"),
                Box::new(Drop),
                has_stash,
            ))
            .child(button(
                "stash-clear",
                IconName::Trash,
                tr("Clear…"),
                Box::new(Clear),
                any,
            ))
    }

    fn render_row(&self, index: usize, focused: bool, cx: &mut Context<Self>) -> AnyElement {
        let ui = Theme::ui(cx);
        let Some(row) = self.rows.get(index) else {
            return div().into_any_element();
        };
        let selected = self.selected.as_ref() == Some(&row.key);
        let now = now_seconds();
        let (glyph, name, name_color, detail): (AnyElement, String, gpui::Hsla, Option<String>) =
            match &row.kind {
                StashRowKind::Repo { name, count } => (
                    icon(IconName::Branch, ui.violet)
                        .size(px(14.))
                        .into_any_element(),
                    name.clone(),
                    ui.foreground,
                    Some(count.to_string()),
                ),
                // The message takes the room; the branch is in the footer and the tooltip.
                StashRowKind::Stash(stash) => (
                    icon(IconName::Stash, ui.amber)
                        .size(px(14.))
                        .into_any_element(),
                    stash_title(stash),
                    ui.foreground,
                    Some(age(now, stash.time)),
                ),
                StashRowKind::File(file) => {
                    let (dir, name) = match file.path.rsplit_once('/') {
                        Some((dir, name)) => (Some(dir.to_string()), name.to_string()),
                        None => (None, file.path.clone()),
                    };
                    (
                        file_icon(&name, &ui).render().into_any_element(),
                        name,
                        git::status_color(file.status, &ui),
                        dir,
                    )
                }
                StashRowKind::Note(text) => (div().into_any_element(), text.clone(), ui.dim, None),
            };
        let strike = matches!(
            &row.kind,
            StashRowKind::File(file) if file.status == FileStatus::Deleted
        );
        let group_row = matches!(row.kind, StashRowKind::Repo { .. });
        let tooltip = match &row.kind {
            StashRowKind::Stash(stash) => {
                Some(format!("{}\n{}", stash.message, stash_place(stash)))
            }
            _ => None,
        };
        let key = row.key.clone();
        let (click, secondary) = (key.clone(), key);
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
            .child(glyph)
            .child(
                div()
                    .min_w_0()
                    // A stash's message takes the room (its age goes to the right edge); a file's
                    // directory follows its name, as in the Commit tab.
                    .when(!matches!(row.kind, StashRowKind::File(_)), |name| {
                        name.flex_1()
                    })
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
                    .max_w(px(120.))
                    .truncate()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.dim)
                    .child(detail)
            }));
        div()
            .id(index)
            .group(ROW_GROUP)
            .h(px(ROW_HEIGHT))
            .w_full()
            .px(px(ROW_INSET))
            .whitespace_nowrap()
            .when_some(tooltip, |row, text| row.tooltip(ui::tooltip(text, None)))
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

    /// No stashes, no repository, or still reading.
    fn render_empty(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let git = self.git.read(cx);
        let (glyph, title, hint, button) =
            if git.is_discovering() || (self.loading && self.rows.is_empty()) {
                (IconName::Refresh, tr("Reading stashes…"), None, false)
            } else if git.repos().is_empty() {
                (
                    IconName::Branch,
                    tr("No Git repository"),
                    Some(tr("Open a folder with a repository: ⌘O")),
                    false,
                )
            } else {
                (
                    IconName::Stash,
                    tr("No stashes"),
                    Some(tr("Put local changes aside to come back to them later")),
                    true,
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
            .when(button, |empty| {
                empty.child(div().pt_1().child(
                    ui::text_button("stash-empty-new", tr("Stash Changes…"), false, ui).on_click(
                        |_, window, cx| window.dispatch_action(Box::new(git::StashChanges), cx),
                    ),
                ))
            })
    }

    /// Under the list: the selected stash (its name and branch) and Apply / Pop.
    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let selected = self.selected_stash();
        let enabled = selected.is_some();
        let caption = selected.as_ref().map(|(_, stash)| stash_place(stash));
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
                    .h(px(18.))
                    .px_1()
                    .truncate()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.dim)
                    .child(caption.unwrap_or_else(|| tr("Select a stash").to_string())),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        ui::primary_button("stash-apply", tr("Apply"), enabled, ui)
                            .tooltip(ui::tooltip(
                                tr("Apply the stash and keep it in the list"),
                                None,
                            ))
                            .when(enabled, |button| {
                                button.on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.apply(false, window, cx)
                                }))
                            }),
                    )
                    .child(
                        ui::text_button("stash-pop", tr("Pop"), false, ui)
                            .tooltip(ui::tooltip(
                                tr("Apply the stash and drop it from the list"),
                                None,
                            ))
                            .when(!enabled, |button| button.opacity(0.5))
                            .when(enabled, |button| {
                                button.on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.apply(true, window, cx)
                                }))
                            }),
                    ),
            )
    }
}

impl Focusable for StashPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for StashPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                "stash-rows",
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
            div()
                .flex_none()
                .px_3()
                .pb_1p5()
                .child(ui::hint_bar(&[("↵", tr("diff")), ("⌫", tr("drop"))], ui).gap_3())
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .key_context("StashList")
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
                    .on_action(cx.listener(Self::open))
                    .on_action(
                        cx.listener(|this, _: &Apply, window, cx| this.apply(false, window, cx)),
                    )
                    .on_action(
                        cx.listener(|this, _: &Pop, window, cx| this.apply(true, window, cx)),
                    )
                    .on_action(cx.listener(Self::unstash_as_branch))
                    .on_action(cx.listener(Self::drop_stash))
                    .on_action(cx.listener(Self::clear))
                    .on_action(cx.listener(Self::refresh))
                    .on_action(cx.listener(Self::show_context_menu))
                    .on_action(
                        cx.listener(|_, _: &Cancel, _, cx| cx.emit(StashPanelEvent::FocusEditor)),
                    )
                    .child(
                        div()
                            .id("stash-list")
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
            // Apply / Pop when there is a stash to act on; the empty state has its own button.
            .when(!self.rows.is_empty(), |panel| {
                panel
                    .child(ui::divider(ui).mx(px(ui::GAP)))
                    .child(self.render_footer(cx))
            })
            .children(
                self.menu
                    .as_ref()
                    .map(|menu| ContextMenu::overlay(&menu.menu, menu.position)),
            )
    }
}

/// A node's chevron (right when closed, down when open); for a file, an empty column.
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

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

/// How long ago, shortly: "just now", "5 min", "3 h", "2 d".
fn age(now: i64, time: i64) -> String {
    let seconds = (now - time).max(0);
    match seconds {
        0..60 => tr("just now").to_string(),
        60..3600 => trf("{0} min", &[&(seconds / 60)]),
        3600..86_400 => trf("{0} h", &[&(seconds / 3600)]),
        _ => trf("{0} d", &[&(seconds / 86_400)]),
    }
}

// --- Flows ---

/// Applies (`pop` — and drops) a stash: the result in a notification; conflicts open the Conflicts
/// dialog, and the stash stays in the list.
pub(crate) fn unstash(
    git: Entity<GitStore>,
    repo: usize,
    stash: Stash,
    pop: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let task = git.update(cx, |git, cx| git.unstash(repo, &stash.oid, pop, false, cx));
    window
        .spawn(cx, async move |cx| {
            let result = task.await;
            report_unstash(&git, repo, &stash, pop, result, cx).await;
        })
        .detach();
}

/// The notification after an unstash (also of Unstash as Branch).
async fn report_unstash(
    git: &Entity<GitStore>,
    repo: usize,
    stash: &Stash,
    pop: bool,
    result: Result<Outcome, GitError>,
    cx: &mut AsyncWindowContext,
) {
    let name = stash_name(stash);
    let title = stash_title(stash);
    match result {
        Ok(Outcome::Conflicts) => {
            let count = conflicted_count(git, repo, cx).await;
            cx.update(|window, cx| {
                // As in JetBrains IDEs: the Conflicts dialog right away.
                window.dispatch_action(Box::new(git::ResolveConflicts), cx);
                let mut notification = Notification::warning(match count {
                    Some(count) => trf(
                        "Unstash: {0}; the stash is kept",
                        &[&trn(
                            count,
                            "conflicts in {n} file",
                            "conflicts in {n} files",
                        )],
                    ),
                    None => tr("Unstash: conflicts; the stash is kept").to_string(),
                })
                .body(stash_label(stash))
                .action(tr("Resolve…"), git::ResolveConflicts);
                notification = notification.action(
                    tr("Drop Stash"),
                    git::DropStash {
                        repo,
                        oid: stash.oid.clone(),
                    },
                );
                git.update(cx, |git, cx| git.notify(notification, cx));
            })
            .ok();
        }
        Ok(_) => {
            let message = if pop {
                trf("Popped {0}", &[&name])
            } else {
                trf("Applied {0}", &[&name])
            };
            git.update(cx, |git, cx| {
                git.notify(Notification::success(message).body(title), cx)
            })
            .ok();
        }
        Err(err) => {
            git.update(cx, |git, cx| {
                git.notify_error(tr("Couldn't unstash the changes"), &err, cx)
            })
            .ok();
        }
    }
}

/// How many files of a repository are conflicted now (read from git: the hub's status may not be
/// fresh yet).
async fn conflicted_count(
    git: &Entity<GitStore>,
    repo: usize,
    cx: &mut AsyncWindowContext,
) -> Option<usize> {
    let task = git
        .update(cx, |git, cx| {
            git.read(repo, cx, |repo| {
                repo.git()
                    .read_only()
                    .args(["diff", "--name-only", "-z", "--diff-filter=U"])
                    .output_string()
            })
        })
        .ok()?;
    let output = task.await.ok()?;
    let count = output.split('\0').filter(|path| !path.is_empty()).count();
    (count > 0).then_some(count)
}

/// Drop: a question naming the stash, then the stash goes.
pub(crate) fn confirm_drop(
    git: Entity<GitStore>,
    repo: usize,
    stash: Stash,
    window: &mut Window,
    cx: &mut App,
) {
    let name = stash_name(&stash);
    let answer = Dialog::warning(trf("Drop {0}?", &[&name]))
        .message(format!(
            "{}\n\n{}",
            stash_title(&stash),
            tr("The stashed changes will be deleted. This can't be undone.")
        ))
        .danger(tr("Drop"))
        .cancel(tr("Cancel"))
        .show(window, cx);
    cx.spawn(async move |cx| {
        if answer.await != Some(0) {
            return;
        }
        let Ok(task) = git.update(cx, |git, cx| git.drop_stash(repo, &stash.oid, cx)) else {
            return;
        };
        let result = task.await;
        git.update(cx, |git, cx| match result {
            Ok(()) => git.notify(
                Notification::success(trf("Dropped {0}", &[&name])).body(stash_title(&stash)),
                cx,
            ),
            Err(err) => git.notify_error(tr("Couldn't drop the stash"), &err, cx),
        })
        .ok();
    })
    .detach();
}

/// The stash of a repository by its commit, from the hub (read if it isn't yet: `None` then).
fn stash_by_oid(git: &Entity<GitStore>, repo: usize, oid: &str, cx: &mut App) -> Option<Stash> {
    git.update(cx, |git, cx| git.stashes(repo, cx))?
        .iter()
        .find(|stash| stash.oid == oid)
        .cloned()
}

/// Drop of a stash named by its commit (a notification's "Drop Stash"): the stash list is read
/// afresh — the Stash tab may never have been shown — then the question.
fn drop_by_oid(git: Entity<GitStore>, repo: usize, oid: String, window: &mut Window, cx: &mut App) {
    let read = git.update(cx, |git, cx| git.read(repo, cx, flux_git::stashes));
    window
        .spawn(cx, async move |cx| {
            let stash = read
                .await
                .ok()
                .and_then(|stashes| stashes.into_iter().find(|stash| stash.oid == oid));
            cx.update(|window, cx| match stash {
                Some(stash) => confirm_drop(git, repo, stash, window, cx),
                None => {
                    let message = tr("The stash is already gone");
                    git.update(cx, |git, cx| {
                        git.report(git::GitEvent::Message(message.into()), cx)
                    })
                }
            })
            .ok();
        })
        .detach();
}

/// Unstash as Branch…: a name for the new branch (git's rules, not an existing branch), then `git
/// stash branch`.
fn unstash_to_branch(
    workspace: &mut Workspace,
    action: &UnstashToBranch,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let git = workspace.git().clone();
    let repo = action.repo;
    let Some(stash) = stash_by_oid(&git, repo, &action.oid, cx) else {
        return;
    };
    let refs = git.read(cx).refs(repo);
    let subtitle = trf("from {0}", &[&stash_label(&stash)]);
    workspace.toggle_dialog(window, cx, move |window, cx| {
        let validate_refs = refs.clone();
        InputDialog::new(tr("Unstash as Branch"), tr("Branch name"), window, cx)
            .subtitle(subtitle)
            .confirm_label(tr("Create Branch"))
            .validate(
                move |name, _, _| {
                    flux_git::check_branch_name(name)
                        .map_err(|reason| SharedString::from(tr(reason).to_string()))?;
                    if validate_refs.local(name).is_some() {
                        return Err(trf("Branch “{0}” already exists", &[&name]).into());
                    }
                    Ok(())
                },
                cx,
            )
            .on_confirm(move |name, _, window, cx| {
                let task = git.update(cx, |git, cx| git.stash_branch(repo, &stash.oid, &name, cx));
                window
                    .spawn(cx, async move |cx| {
                        let result = task.await;
                        match result {
                            Ok(Outcome::Conflicts) => {
                                report_unstash(&git, repo, &stash, true, result, cx).await
                            }
                            Ok(_) => {
                                git.update(cx, |git, cx| {
                                    git.notify(
                                        Notification::success(trf(
                                            "Checked out a new branch {0}",
                                            &[&name],
                                        ))
                                        .body(trf(
                                            "with the changes of {0}",
                                            &[&stash_label(&stash)],
                                        )),
                                        cx,
                                    )
                                })
                                .ok();
                            }
                            Err(err) => {
                                git.update(cx, |git, cx| {
                                    git.notify_error(tr("Couldn't unstash as a branch"), &err, cx)
                                })
                                .ok();
                            }
                        }
                    })
                    .detach();
            })
    });
}

// --- The Stash Changes dialog ---

/// A repository the dialog can stash in.
struct DialogRepo {
    index: usize,
    name: String,
    branch: Option<String>,
    /// Tracked changes and untracked files.
    tracked: usize,
    untracked: usize,
}

/// The Stash Changes dialog, as in JetBrains IDEs: the repository (with several), the message,
/// "Include untracked files" and "Keep index"; or only the files selected in the commit window.
pub struct StashDialog {
    git: Entity<GitStore>,
    repos: Vec<DialogRepo>,
    /// Index into `repos`.
    chosen: usize,
    /// Only these paths (relative); empty — every change.
    paths: Vec<String>,
    message: Entity<TextInput>,
    include_untracked: bool,
    keep_index: bool,
    _subscription: Subscription,
}

impl EventEmitter<DismissEvent> for StashDialog {}

impl StashDialog {
    fn new(
        git: Entity<GitStore>,
        repo: usize,
        paths: Vec<String>,
        include_untracked: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (repos, chosen) = {
            let store = git.read(cx);
            let changes = store.changes();
            let repos: Vec<DialogRepo> = (0..store.repos().len())
                .map(|index| {
                    let count = |untracked: bool| {
                        changes
                            .iter()
                            .filter(|change| {
                                change.repo == index
                                    && (change.status == FileStatus::Untracked) == untracked
                            })
                            .count()
                    };
                    DialogRepo {
                        index,
                        name: store.repo_name(index),
                        branch: store.repos()[index].status.branch.label(),
                        tracked: count(false),
                        untracked: count(true),
                    }
                })
                .collect();
            // The asked repository; if it has nothing to stash, the first one that has.
            let has_changes = |entry: &DialogRepo| entry.tracked + entry.untracked > 0;
            let chosen = repos
                .iter()
                .position(|entry| entry.index == repo && has_changes(entry))
                .or_else(|| repos.iter().position(has_changes))
                .or_else(|| repos.iter().position(|entry| entry.index == repo))
                .unwrap_or(0);
            (repos, chosen)
        };
        let message = cx.new(|cx| TextInput::new(tr("Message (optional)"), cx).code());
        let subscription = cx.subscribe_in(&message, window, |_, _, event, _, cx| match event {
            InputEvent::Changed => cx.notify(),
        });
        Self {
            git,
            repos,
            chosen,
            paths,
            message,
            include_untracked,
            keep_index: false,
            _subscription: subscription,
        }
    }

    /// Stashes and closes; the result is a notification.
    fn confirm(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.repos.get(self.chosen) else {
            return;
        };
        let repo = entry.index;
        let count = if self.paths.is_empty() {
            entry.tracked
                + if self.include_untracked {
                    entry.untracked
                } else {
                    0
                }
        } else {
            self.paths.len()
        };
        let request = StashRequest {
            message: self.message.read(cx).text().trim().to_string(),
            include_untracked: self.include_untracked,
            keep_index: self.keep_index,
            paths: self.paths.clone(),
        };
        let git = self.git.clone();
        let task = git.update(cx, |git, cx| git.stash(repo, request, cx));
        cx.spawn(async move |_, cx| {
            let result = task.await;
            git.update(cx, |git, cx| match result {
                Ok(true) => git.notify(
                    Notification::success(trn(
                        count.max(1),
                        "Stashed {n} file",
                        "Stashed {n} files",
                    )),
                    cx,
                ),
                Ok(false) => git.notify(Notification::info(tr("No local changes to stash")), cx),
                Err(err) => git.notify_error(tr("Couldn't stash the changes"), &err, cx),
            })
            .ok();
        })
        .detach();
        cx.emit(DismissEvent);
    }

    /// ↑↓: the previous / next repository (when the dialog offers a choice).
    fn choose(&mut self, step: isize, cx: &mut Context<Self>) {
        if self.repos.len() < 2 || !self.paths.is_empty() {
            return;
        }
        let last = self.repos.len() as isize - 1;
        self.chosen = (self.chosen as isize + step).clamp(0, last) as usize;
        cx.notify();
    }

    fn render_repos(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .children(self.repos.iter().enumerate().map(|(slot, entry)| {
                let selected = slot == self.chosen;
                let changes = entry.tracked + entry.untracked;
                div()
                    .id(("stash-dialog-repo", slot))
                    .h(px(28.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(px(RADIUS_SM))
                    .cursor_pointer()
                    .when(selected, |row| row.bg(ui.list_selected_inactive))
                    .when(!selected, |row| row.hover(move |style| style.bg(ui.hover)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.chosen = slot;
                        cx.notify();
                    }))
                    .child(ui::radio(("stash-dialog-radio", slot), selected, ui))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child(entry.name.clone()),
                    )
                    .children(entry.branch.clone().map(|branch| {
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.violet)
                            .child(icon(IconName::Branch, ui.violet).size(px(12.)))
                            .child(branch)
                    }))
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.dim)
                            .child(trn(changes, "{n} change", "{n} changes")),
                    )
            }))
    }
}

// Focus is the message field's: nothing else in the dialog takes it, so clicks on the repositories
// and checkboxes keep typing in the field (and don't count as leaving the dialog).
impl Focusable for StashDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.message.focus_handle(cx)
    }
}

impl Render for StashDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let entry = self.repos.get(self.chosen);
        let subtitle = match (entry, self.repos.len() > 1 && self.paths.is_empty()) {
            (Some(entry), false) => Some(match &entry.branch {
                Some(branch) => format!("{} · {}", entry.name, trf("on {0}", &[branch])),
                None => entry.name.clone(),
            }),
            _ => None,
        };
        let files = (!self.paths.is_empty()).then(|| {
            let mut names: Vec<String> = self
                .paths
                .iter()
                .take(LISTED_FILES)
                .map(|path| path.rsplit('/').next().unwrap_or(path).to_string())
                .collect();
            if self.paths.len() > LISTED_FILES {
                names.push(trf("and {0} more", &[&(self.paths.len() - LISTED_FILES)]));
            }
            format!(
                "{}: {}",
                trn(self.paths.len(), "{n} file", "{n} files"),
                names.join(", ")
            )
        });
        let check = |id: &'static str, label: &'static str, on: bool| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap_1p5()
                .cursor_pointer()
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.text_muted)
                .hover(move |style| style.text_color(ui.foreground))
                .child(ui::checkbox(
                    SharedString::from(format!("{id}-box")),
                    CheckState::from_bool(on),
                    ui,
                ))
                .child(label)
        };
        ui::popover(ui)
            .key_context("StashDialog")
            .on_action(cx.listener(|this, _: &Confirm, window, cx| this.confirm(window, cx)))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(DismissEvent)))
            .on_action(cx.listener(|this, _: &NextRepo, _, cx| this.choose(1, cx)))
            .on_action(cx.listener(|this, _: &PreviousRepo, _, cx| this.choose(-1, cx)))
            .w(px(DIALOG_WIDTH))
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(icon(IconName::Stash, ui.text_muted).size(px(15.)))
                            .child(tr("Stash Changes")),
                    )
                    .children(subtitle.map(|subtitle| {
                        div()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.dim)
                            .child(subtitle)
                    }))
                    .children(files.map(|files| {
                        div()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.dim)
                            .truncate()
                            .child(files)
                    })),
            )
            .when(self.repos.len() > 1 && self.paths.is_empty(), |dialog| {
                dialog.child(self.render_repos(cx))
            })
            .child(self.message.clone())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(
                        check(
                            "stash-dialog-untracked",
                            tr("Include untracked files"),
                            self.include_untracked,
                        )
                        .on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| {
                                this.include_untracked = !this.include_untracked;
                                cx.notify();
                            },
                        )),
                    )
                    .child(
                        check("stash-dialog-keep-index", tr("Keep index"), self.keep_index)
                            .tooltip(ui::tooltip(
                                tr("Staged changes stay in the working tree too"),
                                None,
                            ))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.keep_index = !this.keep_index;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        ui::text_button("stash-dialog-cancel", tr("Cancel"), false, ui)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    )
                    .child(
                        ui::primary_button("stash-dialog-ok", tr("Create Stash"), true, ui)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.confirm(window, cx)
                            })),
                    ),
            )
    }
}

/// Opens the Stash Changes dialog: for `repo` (`None` — the active file's repository) and `paths`
/// (empty — every change).
fn open_dialog(
    workspace: &mut Workspace,
    repo: Option<usize>,
    paths: Vec<String>,
    include_untracked: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let git = workspace.git().clone();
    let active = workspace.active_path(cx);
    let repo = repo.or_else(|| git.read(cx).current_repo(active.as_deref()));
    let Some(repo) = repo else {
        let message = tr("No Git repository");
        return git.update(cx, |git, cx| {
            git.report(git::GitEvent::Message(message.into()), cx)
        });
    };
    workspace.toggle_dialog(window, cx, move |window, cx| {
        StashDialog::new(git, repo, paths, include_untracked, window, cx)
    });
}

/// The window-level actions: `StashChanges` (the dialog), `StashSelected` (the dialog for the
/// commit window's selection), `UnstashChanges` (the Stash tab), `DropStash` (with the question),
/// `UnstashToBranch` (the name dialog).
pub fn workspace_actions(root: Div, cx: &mut Context<Workspace>) -> Div {
    root.on_action(cx.listener(|this, _: &git::StashChanges, window, cx| {
        open_dialog(this, None, Vec::new(), false, window, cx)
    }))
    .on_action(cx.listener(|this, action: &StashSelected, window, cx| {
        open_dialog(
            this,
            Some(action.repo),
            action.paths.clone(),
            action.untracked,
            window,
            cx,
        )
    }))
    .on_action(cx.listener(|this, _: &git::UnstashChanges, window, cx| this.show_stash(window, cx)))
    .on_action(cx.listener(|this, action: &git::DropStash, window, cx| {
        drop_by_oid(
            this.git().clone(),
            action.repo,
            action.oid.clone(),
            window,
            cx,
        )
    }))
    .on_action(cx.listener(|this, action: &UnstashToBranch, window, cx| {
        unstash_to_branch(this, action, window, cx)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stash(index: usize, oid: &str, message: &str, branch: Option<&str>) -> Stash {
        Stash {
            index,
            oid: oid.into(),
            message: message.into(),
            branch: branch.map(Into::into),
            time: 0,
        }
    }

    fn change(status: FileStatus, path: &str, orig: Option<&str>) -> FileChange {
        FileChange {
            status,
            path: path.into(),
            orig_path: orig.map(Into::into),
        }
    }

    fn labels(rows: &[StashRow]) -> Vec<String> {
        rows.iter()
            .map(|row| {
                let text = match &row.kind {
                    StashRowKind::Repo { name, count } => format!("repo {name} {count}"),
                    StashRowKind::Stash(stash) => format!("stash {}", stash_title(stash)),
                    StashRowKind::File(file) => format!("file {}", file.path),
                    StashRowKind::Note(text) => format!("note {text}"),
                };
                format!("{}{text}", "  ".repeat(row.depth))
            })
            .collect()
    }

    #[test]
    fn stashes_open_to_their_files_sorted_by_path() {
        let stashes = vec![
            stash(0, "aaa", "Parser experiment", Some("main")),
            stash(1, "bbb", "WIP on main: 1234567 Fix", Some("main")),
        ];
        let repos = [RepoStashes {
            repo: 0,
            name: "demo".into(),
            stashes: &stashes,
        }];
        let expanded: HashSet<(usize, String)> =
            [(0, "aaa".to_string()), (0, "bbb".to_string())].into();
        let mut files = HashMap::new();
        files.insert(
            (0, "aaa".to_string()),
            StashFiles::Loaded(vec![
                change(FileStatus::Untracked, "notes/idea.md", None),
                change(FileStatus::Modified, "src/parser.rs", None),
                change(FileStatus::Modified, "README.md", None),
            ]),
        );
        let rows = build_rows(&repos, false, &expanded, &HashSet::new(), &files);
        assert_eq!(
            labels(&rows),
            vec![
                "stash Parser experiment",
                "  file notes/idea.md",
                "  file README.md",
                "  file src/parser.rs",
                "stash WIP on main: 1234567 Fix",
                "  note Reading…",
            ]
        );
        assert!(rows[0].expanded);
        assert_eq!(
            rows[2].key,
            StashKey::File(0, "aaa".into(), "README.md".into())
        );
    }

    #[test]
    fn several_repositories_get_nodes_and_empty_ones_are_skipped() {
        let first = vec![stash(0, "aaa", "One", None)];
        let second: Vec<Stash> = Vec::new();
        let third = vec![stash(0, "ccc", "Three", Some("dev"))];
        let repos = [
            RepoStashes {
                repo: 0,
                name: "app".into(),
                stashes: &first,
            },
            RepoStashes {
                repo: 1,
                name: "docs".into(),
                stashes: &second,
            },
            RepoStashes {
                repo: 2,
                name: "lib".into(),
                stashes: &third,
            },
        ];
        let collapsed: HashSet<usize> = [2].into();
        let rows = build_rows(&repos, true, &HashSet::new(), &collapsed, &HashMap::new());
        assert_eq!(
            labels(&rows),
            vec!["repo app 1", "  stash One", "repo lib 1"]
        );
        assert!(!rows[2].expanded);
        assert_eq!(rows[1].repo(), 0);
    }

    #[test]
    fn stash_files_diff_against_the_commit_they_were_made_on() {
        let stash = stash(2, "abc", "Experiment", Some("main"));
        let modified = change(FileStatus::Modified, "src/a.rs", None);
        let (left, right) = diff_sides(&stash, &modified);
        assert!(matches!(left, DiffSide::Revision { ref rev, .. } if rev == "abc^1"));
        assert!(
            matches!(right, DiffSide::Revision { ref rev, ref path, .. } if rev == "abc" && path == "src/a.rs")
        );
        let untracked = change(FileStatus::Untracked, "notes/idea.md", None);
        let (_, right) = diff_sides(&stash, &untracked);
        assert!(matches!(right, DiffSide::Revision { ref rev, .. } if rev == "abc^3"));
        // A renamed file: its old path on the left.
        let renamed = change(FileStatus::Renamed, "src/new.rs", Some("src/old.rs"));
        let (left, right) = diff_sides(&stash, &renamed);
        assert!(matches!(left, DiffSide::Revision { ref path, .. } if path == "src/old.rs"));
        assert!(matches!(right, DiffSide::Revision { ref path, .. } if path == "src/new.rs"));
        assert_eq!(stash_name(&stash), "stash@{2}");
        assert_eq!(stash_title(&self::stash(0, "x", "  ", None)), "stash@{0}");
    }

    #[test]
    fn ages_are_short() {
        assert_eq!(age(100, 90), "just now");
        assert_eq!(age(10_000, 10_000 - 300), "5 min");
        assert_eq!(age(100_000, 100_000 - 7200), "2 h");
        assert_eq!(age(1_000_000, 1_000_000 - 3 * 86_400), "3 d");
    }
}
