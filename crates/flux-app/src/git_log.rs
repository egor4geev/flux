//! The log of the Git window (part B of stage 6.3), as JetBrains' Git tool window → Log: on the
//! left, the branches (HEAD, Local, Remote, Tags — a click filters the log by a branch, a double
//! click checks it out); in the middle, the filter bar (repository, branch, user, date, paths, text
//! or hash) and the commits with the graph, branch and tag labels, the author and the date; on the
//! right, the commit pane (`commit_details`).
//!
//! The same view shows a file's history or the history of a range of its lines
//! ([`LogScope::File`], [`LogScope::Lines`]): no graph and no branches, and ↵ (⌘D, a double click)
//! opens the diff of the file in the selected commit.
//!
//! Commits are read page by page in the background (`flux_git::log`), the next page when the list
//! is scrolled near its end; the log is read again when the repository's refs change (a commit, a
//! checkout, a fetch), keeping the selection.
//!
//! Keys: ↑↓ (⇧ — extend), PageUp / PageDown, Home / End, ⌘A, ⌘C (copy hashes), ⌘F (search), ↵ / ⌘D
//! (a history's diff), the context menu of commits (`log_actions`).

use std::collections::{BTreeSet, HashMap};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use flux_git::{FileRevision, FileStatus, GitError, LogCommit, LogFilter, RefKind, Refs};
use gpui::{
    Action, AnyElement, App, ClickEvent, ClipboardItem, Context, DismissEvent, Entity,
    EventEmitter, FocusHandle, Focusable, FontWeight, Hsla, KeyBinding, MouseButton,
    MouseDownEvent, Pixels, Point, Render, ScrollStrategy, SharedString, Subscription, Task,
    UniformListScrollHandle, Window, actions, div, prelude::*, px, uniform_list,
};

use crate::commit_details::CommitDetailsView;
use crate::context_menu::ContextMenu;
use crate::diff_view::DiffSide;
use crate::git::{self, GitStore};
use crate::git_graph::{self, GraphLayout};
use crate::i18n::{tr, trf};
use crate::icons::{IconName, icon};
use crate::input::{InputEvent, TextInput};
use crate::push_dialog::now_seconds;
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, RADIUS_SM};

/// The commits selected in a log, for the commit menu and the operations on them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogSelection {
    pub repo: usize,
    /// Newest first, as the log shows them.
    pub commits: Vec<LogCommit>,
    /// A file's history: the file's path (relative) at the first selected commit.
    pub path: Option<String>,
}

actions!(
    git_log,
    [
        SelectNext,
        SelectPrevious,
        ExtendNext,
        ExtendPrevious,
        PageDown,
        PageUp,
        SelectFirst,
        SelectLast,
        SelectAll,
        CopyHash,
        OpenDiff,
        FocusSearch,
        ToggleBranches,
        ResetFilters,
        Refresh,
    ]
);

/// A filter chosen in a filter menu.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git_log, no_json)]
pub struct SetBranchFilter {
    pub branch: Option<String>,
}

#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git_log, no_json)]
pub struct SetUserFilter {
    pub user: Option<String>,
}

/// The last `days` days; `None` — any date.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git_log, no_json)]
pub struct SetDateFilter {
    pub days: Option<u32>,
}

/// Type a filter's value in the filter bar (a user, a date, a path).
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git_log, no_json)]
pub struct EditFilter {
    pub kind: FilterKind,
}

#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git_log, no_json)]
pub struct SetRepo {
    pub repo: usize,
}

/// A filter whose value is typed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FilterKind {
    User,
    Since,
    Path,
}

/// Commits read at a time.
const PAGE: usize = 1000;
/// The next page is read when the list shows a row this close to its end.
const PREFETCH: usize = 200;
const ROW_HEIGHT: f32 = 26.;
const BRANCHES_WIDTH: f32 = 190.;
const DETAILS_WIDTH: f32 = 320.;
const AUTHOR_WIDTH: f32 = 96.;
const DATE_WIDTH: f32 = 112.;
/// Below this window width the branches pane isn't shown.
const NARROW_WINDOW: f32 = 1400.;
/// Labels shown on a commit's row before "+N".
const MAX_LABELS: usize = 2;
const FILTER_BAR_HEIGHT: f32 = 36.;
/// Typing in the search field waits this long before the log is read again.
const SEARCH_DELAY: Duration = Duration::from_millis(300);
/// Ref changes are coalesced for this long before the log is read again.
const REFRESH_DELAY: Duration = Duration::from_millis(150);

pub fn init(cx: &mut App) {
    let context = Some("GitLog");
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("up", SelectPrevious, context),
        KeyBinding::new("shift-down", ExtendNext, context),
        KeyBinding::new("shift-up", ExtendPrevious, context),
        KeyBinding::new("pagedown", PageDown, context),
        KeyBinding::new("pageup", PageUp, context),
        KeyBinding::new("home", SelectFirst, context),
        KeyBinding::new("end", SelectLast, context),
        KeyBinding::new("cmd-a", SelectAll, context),
        KeyBinding::new("cmd-c", CopyHash, context),
        KeyBinding::new("enter", OpenDiff, context),
        KeyBinding::new("cmd-d", OpenDiff, context),
        KeyBinding::new("cmd-f", FocusSearch, context),
    ]);
}

/// What a log shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogScope {
    /// The repository's log, with the graph and the filters.
    All,
    /// A file's history (relative path, as HEAD has it).
    File { path: String },
    /// The history of lines `start..=end` (1-based) of a file.
    Lines { path: String, start: u32, end: u32 },
}

impl LogScope {
    fn path(&self) -> Option<&str> {
        match self {
            LogScope::All => None,
            LogScope::File { path } | LogScope::Lines { path, .. } => Some(path),
        }
    }
}

/// A branch or tag label on a commit.
#[derive(Debug, Clone)]
struct Label {
    name: SharedString,
    kind: RefKind,
    current: bool,
}

/// The value being typed in the filter bar.
struct Editing {
    kind: FilterKind,
    input: Entity<TextInput>,
    _subscription: Subscription,
}

/// A context menu open at a point.
struct Menu {
    menu: Entity<ContextMenu>,
    position: Point<Pixels>,
    _subscriptions: [Subscription; 2],
}

pub struct GitLogView {
    git: Entity<GitStore>,
    repo: usize,
    scope: LogScope,
    filter: LogFilter,
    commits: Vec<LogCommit>,
    /// A history's revisions (the file's path at each commit), parallel to `commits`.
    revisions: Vec<FileRevision>,
    graph: GraphLayout,
    /// Pages of an older reading are dropped.
    generation: u64,
    loading: bool,
    /// The last page was short: there is nothing more to read.
    exhausted: bool,
    error: Option<SharedString>,
    load_task: Option<Task<()>>,
    /// Selected rows; `cursor` is the one the keys move, `anchor` where a ⇧-range starts.
    selected: BTreeSet<usize>,
    cursor: Option<usize>,
    anchor: Option<usize>,
    /// After a reading: the commits to select again (by hash), and whether to scroll to them.
    reselect: Vec<String>,
    /// A commit to show once its page is read (Show Commit in Log).
    pending: Option<String>,
    scroll: UniformListScrollHandle,
    details: Entity<CommitDetailsView>,
    details_shown: Option<(usize, Vec<String>)>,
    focus_handle: FocusHandle,
    /// The branches pane is shown: the user's choice, or by default only in a wide window (in a
    /// narrow one it gives its room to the commits' messages).
    branches_open: Option<bool>,
    /// The window was wide in the last frame.
    wide: bool,
    collapsed: BTreeSet<&'static str>,
    search: Entity<TextInput>,
    search_task: Option<Task<()>>,
    editing: Option<Editing>,
    menu: Option<Menu>,
    /// Labels of commits, by hash, from the refs last seen.
    labels: HashMap<String, Vec<Label>>,
    refs_seen: Option<Arc<Refs>>,
    head: Option<String>,
    /// The user's name (`git config user.name`): "me" in the User filter.
    me: Option<String>,
    refresh_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DismissEvent> for GitLogView {}

impl GitLogView {
    pub fn new(
        git: Entity<GitStore>,
        repo: usize,
        scope: LogScope,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let details = cx.new(|cx| CommitDetailsView::new(git.clone(), window, cx));
        let search = cx.new(|cx| {
            TextInput::new(tr("Text or hash"), cx)
                .icon(IconName::Search)
                .compact()
        });
        let subscriptions = vec![
            cx.observe(&git, |this, _, cx| this.git_changed(cx)),
            cx.subscribe(&search, |this, _, event: &InputEvent, cx| match event {
                InputEvent::Changed => this.search_changed(cx),
            }),
        ];
        let mut view = Self {
            git,
            repo,
            scope,
            filter: LogFilter::default(),
            commits: Vec::new(),
            revisions: Vec::new(),
            graph: GraphLayout::new(),
            generation: 0,
            loading: false,
            exhausted: false,
            error: None,
            load_task: None,
            selected: BTreeSet::new(),
            cursor: None,
            anchor: None,
            reselect: Vec::new(),
            pending: None,
            scroll: UniformListScrollHandle::new(),
            details,
            details_shown: None,
            focus_handle: cx.focus_handle(),
            branches_open: None,
            wide: true,
            collapsed: BTreeSet::new(),
            search,
            search_task: None,
            editing: None,
            menu: None,
            labels: HashMap::new(),
            refs_seen: None,
            head: None,
            me: None,
            refresh_task: None,
            _subscriptions: subscriptions,
        };
        view.read_labels(cx);
        view.read_me(cx);
        view.reload(cx);
        view
    }

    pub fn scope(&self) -> &LogScope {
        &self.scope
    }

    pub fn repo(&self) -> usize {
        self.repo
    }

    /// The tab's caption: "Log", "History: main.rs", "History: main.rs:10–20".
    pub fn title(&self) -> SharedString {
        let name = |path: &str| path.rsplit('/').next().unwrap_or(path).to_string();
        match &self.scope {
            LogScope::All => tr("Log").into(),
            LogScope::File { path } => trf("History: {0}", &[&name(path)]).into(),
            LogScope::Lines { path, start, end } => {
                trf("History: {0}", &[&format!("{}:{start}–{end}", name(path))]).into()
            }
        }
    }

    /// The tab's tooltip: the repository-relative path of a history.
    pub fn tooltip(&self) -> Option<SharedString> {
        self.scope.path().map(|path| path.to_string().into())
    }

    /// Whether focus is anywhere in the view: the list, the search field, a filter being typed, a
    /// menu, the commit pane.
    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus_handle.contains_focused(window, cx)
            || self.search.focus_handle(cx).is_focused(window)
            || self
                .editing
                .as_ref()
                .is_some_and(|editing| editing.input.focus_handle(cx).is_focused(window))
            || self
                .menu
                .as_ref()
                .is_some_and(|menu| menu.menu.focus_handle(cx).is_focused(window))
            || self.details.focus_handle(cx).contains_focused(window, cx)
    }

    /// Show Commit in Log: the commit is selected and scrolled to; until its page is read, the log
    /// goes on reading (the filters are dropped if they hide it).
    pub fn show_commit(&mut self, oid: &str, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle);
        if let Some(index) = self.commits.iter().position(|commit| commit.oid == oid) {
            self.select_only(index, cx);
            return;
        }
        self.pending = Some(oid.to_string());
        if self.filter != LogFilter::default() {
            self.filter = LogFilter::default();
            self.search.update(cx, |search, cx| search.set_text("", cx));
            self.search_task = None;
            self.reload(cx);
        } else if !self.loading && !self.exhausted {
            self.load_page(cx);
        } else if self.exhausted {
            self.pending = None;
        }
    }

    // --- Reading ---

    /// Reads the log again from its start, keeping the selected commits selected.
    fn reload(&mut self, cx: &mut Context<Self>) {
        self.reselect = self
            .selected
            .iter()
            .filter_map(|index| self.commits.get(*index))
            .map(|commit| commit.oid.clone())
            .collect();
        self.generation += 1;
        self.commits.clear();
        self.revisions.clear();
        self.graph = GraphLayout::new();
        self.selected.clear();
        self.cursor = None;
        self.anchor = None;
        self.exhausted = false;
        self.error = None;
        self.loading = false;
        self.load_page(cx);
    }

    /// Reads the next page in the background.
    fn load_page(&mut self, cx: &mut Context<Self>) {
        if self.loading || self.exhausted {
            return;
        }
        self.loading = true;
        let generation = self.generation;
        let skip = self.commits.len();
        let scope = self.scope.clone();
        let filter = self.filter.clone();
        let repo = self.repo;
        let read = self.git.update(cx, |git, cx| {
            git.read(repo, cx, move |repo| match &scope {
                LogScope::All => flux_git::log::log(repo, &filter, skip, PAGE)
                    .map(|commits| (commits, Vec::new())),
                LogScope::File { path } => {
                    flux_git::log::file_history(repo, path, skip, PAGE).map(split_revisions)
                }
                LogScope::Lines { path, start, end } => {
                    // `git log -L` reads every revision at once.
                    if skip > 0 {
                        return Ok((Vec::new(), Vec::new()));
                    }
                    let last = lines_in_head(repo, path).unwrap_or(*end);
                    let start = (*start).min(last);
                    let end = (*end).clamp(start, last);
                    flux_git::log::line_history(repo, path, start, end, usize::MAX)
                        .map(split_revisions)
                }
            })
        });
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = read.await;
            this.update(cx, |this, cx| this.page_read(generation, result, cx))
                .ok();
        }));
        cx.notify();
    }

    fn page_read(
        &mut self,
        generation: u64,
        result: Result<(Vec<LogCommit>, Vec<FileRevision>), GitError>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        match result {
            Err(err) => {
                self.error = Some(err.to_string().into());
                self.exhausted = true;
            }
            Ok((commits, revisions)) => {
                let short = match self.scope {
                    LogScope::Lines { .. } => true,
                    _ => commits.len() < PAGE,
                };
                self.exhausted = short;
                if self.scope == LogScope::All {
                    self.graph.extend(&commits);
                }
                self.commits.extend(commits);
                self.revisions.extend(revisions);
            }
        }
        // The commits selected before the reading come back.
        if !self.reselect.is_empty() {
            let reselect = std::mem::take(&mut self.reselect);
            for oid in &reselect {
                if let Some(index) = self.commits.iter().position(|commit| commit.oid == *oid) {
                    self.selected.insert(index);
                    self.cursor.get_or_insert(index);
                }
            }
            self.anchor = self.cursor;
        }
        if let Some(oid) = self.pending.clone() {
            match self.commits.iter().position(|commit| commit.oid == oid) {
                Some(index) => {
                    self.pending = None;
                    self.select_only(index, cx);
                }
                None if !self.exhausted => self.load_page(cx),
                None => self.pending = None,
            }
        }
        // A history selects its newest revision: the commit pane has something to show.
        if self.selected.is_empty() && self.scope != LogScope::All && !self.commits.is_empty() {
            self.selected.insert(0);
            self.cursor = Some(0);
            self.anchor = Some(0);
        }
        self.selection_changed(cx);
        cx.notify();
    }

    /// The hub changed: new refs (a commit, a checkout, a fetch) read the log again.
    fn git_changed(&mut self, cx: &mut Context<Self>) {
        let refs = self.git.read(cx).refs(self.repo);
        if self
            .refs_seen
            .as_ref()
            .is_some_and(|seen| Arc::ptr_eq(seen, &refs))
        {
            return;
        }
        let first = self.refs_seen.is_none()
            || self
                .refs_seen
                .as_ref()
                .is_some_and(|r| r.local.is_empty() && r.tags.is_empty());
        self.read_labels(cx);
        if first && !self.commits.is_empty() {
            // The refs were read for the first time: only the labels change.
            cx.notify();
            return;
        }
        self.refresh_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(REFRESH_DELAY).await;
            this.update(cx, |this, cx| this.reload(cx)).ok();
        }));
    }

    /// The labels of commits from the repository's refs, and HEAD.
    fn read_labels(&mut self, cx: &mut Context<Self>) {
        let refs = self.git.read(cx).refs(self.repo);
        let mut labels: HashMap<String, Vec<Label>> = HashMap::new();
        let mut add =
            |oid: &str, label: Label| labels.entry(oid.to_string()).or_default().push(label);
        for reference in refs.local.iter().chain(&refs.remote).chain(&refs.tags) {
            add(
                &reference.oid,
                Label {
                    name: reference.name.clone().into(),
                    kind: reference.kind,
                    current: reference.current,
                },
            );
        }
        // Current branch first, then local, remote, tags.
        for list in labels.values_mut() {
            list.sort_by_key(|label| {
                (
                    !label.current,
                    match label.kind {
                        RefKind::Local => 0,
                        RefKind::Remote => 1,
                        RefKind::Tag => 2,
                    },
                )
            });
        }
        self.head = refs.current().map(|branch| branch.oid.clone());
        self.labels = labels;
        self.refs_seen = Some(refs);
        if self.head.is_none() {
            // A detached HEAD: its commit isn't a branch's.
            let read = self
                .git
                .update(cx, |git, cx| git.resolve_rev(self.repo, "HEAD", cx));
            cx.spawn(async move |this, cx| {
                if let Ok(Some(oid)) = read.await {
                    this.update(cx, |this, cx| {
                        this.head = Some(oid);
                        cx.notify();
                    })
                    .ok();
                }
            })
            .detach();
        }
    }

    /// `git config user.name`, for "me" in the User filter.
    fn read_me(&mut self, cx: &mut Context<Self>) {
        let read = self.git.update(cx, |git, cx| {
            git.read(self.repo, cx, |repo| {
                repo.git()
                    .read_only()
                    .args(["config", "user.name"])
                    .output_string()
            })
        });
        cx.spawn(async move |this, cx| {
            if let Ok(name) = read.await {
                let name = name.trim().to_string();
                this.update(cx, |this, _| this.me = (!name.is_empty()).then_some(name))
                    .ok();
            }
        })
        .detach();
    }

    // --- Filters ---

    fn set_filter(&mut self, change: impl FnOnce(&mut LogFilter), cx: &mut Context<Self>) {
        let mut filter = self.filter.clone();
        change(&mut filter);
        if filter != self.filter {
            self.filter = filter;
            self.reload(cx);
        }
    }

    fn search_changed(&mut self, cx: &mut Context<Self>) {
        let text = self.search.read(cx).text();
        self.search_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DELAY).await;
            this.update(cx, |this, cx| {
                let text = text.trim();
                let text = (!text.is_empty()).then(|| text.to_string());
                this.set_filter(|filter| filter.text = text, cx);
            })
            .ok();
        }));
    }

    fn reset_filters(&mut self, cx: &mut Context<Self>) {
        self.search.update(cx, |search, cx| search.set_text("", cx));
        self.search_task = None;
        self.editing = None;
        self.set_filter(|filter| *filter = LogFilter::default(), cx);
    }

    /// A filter's value is typed in the filter bar: ↵ applies, Esc cancels.
    fn edit_filter(&mut self, kind: FilterKind, window: &mut Window, cx: &mut Context<Self>) {
        let (placeholder, text) = match kind {
            FilterKind::User => (tr("Name or e-mail"), self.filter.authors.join(", ")),
            FilterKind::Since => (
                tr("Since a date: YYYY-MM-DD"),
                self.filter.since.map(format_day).unwrap_or_default(),
            ),
            FilterKind::Path => (
                tr("Path in the repository: src/, README.md"),
                self.filter.paths.join(", "),
            ),
        };
        let input = cx.new(|cx| {
            let mut input = TextInput::new(placeholder, cx).compact();
            input.set_text(&text, cx);
            input.select_all(cx);
            input
        });
        let subscription = cx.subscribe(&input, |_, _, _: &InputEvent, cx| cx.notify());
        window.focus(&input.focus_handle(cx));
        self.editing = Some(Editing {
            kind,
            input,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// ↵ in the filter being typed.
    fn apply_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.take() else {
            return;
        };
        let text = editing.input.read(cx).text();
        let list = |text: &str| -> Vec<String> {
            text.split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_string)
                .collect()
        };
        match editing.kind {
            FilterKind::User => self.set_filter(|filter| filter.authors = list(&text), cx),
            FilterKind::Path => self.set_filter(
                |filter| {
                    filter.paths = list(&text)
                        .into_iter()
                        .map(|path| path.trim_start_matches("./").to_string())
                        .collect()
                },
                cx,
            ),
            FilterKind::Since => {
                let text = text.trim();
                if text.is_empty() {
                    self.set_filter(|filter| filter.since = None, cx);
                } else if let Some(day) = parse_day(text) {
                    let since = day - local_offset(day);
                    self.set_filter(|filter| filter.since = Some(since), cx);
                } else {
                    // Not a date: the field stays for a fix.
                    self.editing = Some(editing);
                    return;
                }
            }
        }
        window.focus(&self.focus_handle);
        cx.notify();
    }

    fn cancel_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.take().is_some() {
            window.focus(&self.focus_handle);
            cx.notify();
        }
    }

    /// A filter's menu under its chip.
    fn open_filter_menu(
        &mut self,
        which: FilterMenu,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let refs = self.git.read(cx).refs(self.repo);
        let authors = self.frequent_authors();
        let me = self.me.clone();
        let repos: Vec<String> = (0..self.git.read(cx).repos().len())
            .map(|repo| self.git.read(cx).repo_name(repo))
            .collect();
        window.focus(&self.focus_handle);
        let menu = cx.new(|cx| {
            let menu = ContextMenu::new(window, cx);
            match which {
                FilterMenu::Repo => repos.iter().enumerate().fold(menu, |menu, (repo, name)| {
                    menu.entry(name.clone(), SetRepo { repo })
                }),
                FilterMenu::Branch => {
                    let mut menu = menu
                        .entry(tr("All"), SetBranchFilter { branch: None })
                        .entry(
                            "HEAD",
                            SetBranchFilter {
                                branch: Some("HEAD".into()),
                            },
                        )
                        .separator();
                    for branch in refs.local.iter().chain(&refs.remote) {
                        menu = menu.entry(
                            branch.name.clone(),
                            SetBranchFilter {
                                branch: Some(branch.name.clone()),
                            },
                        );
                    }
                    menu
                }
                FilterMenu::User => {
                    let mut menu = menu.entry(tr("All"), SetUserFilter { user: None });
                    if let Some(me) = &me {
                        menu = menu.entry(
                            trf("me ({0})", &[me]),
                            SetUserFilter {
                                user: Some(me.clone()),
                            },
                        );
                    }
                    menu = menu.separator();
                    for author in authors {
                        menu = menu.entry(author.clone(), SetUserFilter { user: Some(author) });
                    }
                    menu.separator().entry(
                        tr("Select…"),
                        EditFilter {
                            kind: FilterKind::User,
                        },
                    )
                }
                FilterMenu::Date => menu
                    .entry(tr("All"), SetDateFilter { days: None })
                    .entry(tr("Last 24 hours"), SetDateFilter { days: Some(1) })
                    .entry(tr("Last 7 days"), SetDateFilter { days: Some(7) })
                    .entry(tr("Last 30 days"), SetDateFilter { days: Some(30) })
                    .separator()
                    .entry(
                        tr("Select…"),
                        EditFilter {
                            kind: FilterKind::Since,
                        },
                    ),
            }
        });
        self.show_menu(menu, position, window, cx);
    }

    /// The authors of the commits read so far, the most frequent first.
    fn frequent_authors(&self) -> Vec<String> {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for commit in &self.commits {
            *counts.entry(commit.author.as_str()).or_default() += 1;
        }
        let mut authors: Vec<(&str, usize)> = counts.into_iter().collect();
        authors.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        authors
            .into_iter()
            .take(12)
            .map(|(author, _)| author.to_string())
            .collect()
    }

    fn set_repo(&mut self, repo: usize, cx: &mut Context<Self>) {
        if repo == self.repo {
            return;
        }
        self.repo = repo;
        self.filter = LogFilter::default();
        self.selected.clear();
        self.refs_seen = None;
        self.read_labels(cx);
        self.read_me(cx);
        self.reload(cx);
    }

    // --- Selection ---

    fn select_only(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.commits.len() {
            return;
        }
        self.selected.clear();
        self.selected.insert(index);
        self.cursor = Some(index);
        self.anchor = Some(index);
        self.scroll.scroll_to_item(index, ScrollStrategy::Center);
        self.selection_changed(cx);
        cx.notify();
    }

    /// The keys: the cursor moves by `step` rows (clamped); `extend` — the range from the anchor.
    fn move_cursor(&mut self, step: isize, extend: bool, cx: &mut Context<Self>) {
        if self.commits.is_empty() {
            return;
        }
        let last = self.commits.len() - 1;
        let to = match self.cursor {
            Some(at) => (at as isize + step).clamp(0, last as isize) as usize,
            None => 0,
        };
        if extend {
            let anchor = self.anchor.unwrap_or(to);
            self.selected = (anchor.min(to)..=anchor.max(to)).collect();
            self.cursor = Some(to);
            self.scroll.scroll_to_item(to, ScrollStrategy::Top);
            self.selection_changed(cx);
            cx.notify();
        } else {
            self.select_only(to, cx);
        }
        if to + PREFETCH >= self.commits.len() {
            self.load_page(cx);
        }
    }

    fn click_row(
        &mut self,
        index: usize,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        let modifiers = event.modifiers();
        if modifiers.shift {
            let anchor = self.anchor.unwrap_or(index);
            self.selected = (anchor.min(index)..=anchor.max(index)).collect();
            self.cursor = Some(index);
        } else if modifiers.platform {
            if !self.selected.remove(&index) {
                self.selected.insert(index);
            }
            self.cursor = Some(index);
            self.anchor = Some(index);
        } else {
            self.selected.clear();
            self.selected.insert(index);
            self.cursor = Some(index);
            self.anchor = Some(index);
            if event.click_count() == 2 {
                self.open_diff(window, cx);
            }
        }
        self.selection_changed(cx);
        cx.notify();
    }

    /// The selection as the operations see it.
    pub fn selection(&self) -> Option<LogSelection> {
        let commits: Vec<LogCommit> = self
            .selected
            .iter()
            .filter_map(|index| self.commits.get(*index).cloned())
            .collect();
        if commits.is_empty() {
            return None;
        }
        let path = self
            .selected
            .first()
            .and_then(|index| self.revisions.get(*index))
            .map(|revision| revision.path.clone());
        Some(LogSelection {
            repo: self.repo,
            commits,
            path,
        })
    }

    /// The commit pane follows the selection.
    fn selection_changed(&mut self, cx: &mut Context<Self>) {
        let oids: Vec<String> = self
            .selected
            .iter()
            .filter_map(|index| self.commits.get(*index))
            .map(|commit| commit.oid.clone())
            .collect();
        let shown = Some((self.repo, oids.clone()));
        if shown == self.details_shown {
            return;
        }
        self.details_shown = shown;
        let path = self
            .selected
            .first()
            .and_then(|index| self.revisions.get(*index))
            .map(|revision| revision.path.clone());
        let repo = self.repo;
        self.details
            .update(cx, |details, cx| details.show(repo, oids, path, cx));
    }

    fn copy_hashes(&mut self, cx: &mut Context<Self>) {
        let hashes: Vec<&str> = self
            .selected
            .iter()
            .filter_map(|index| self.commits.get(*index))
            .map(|commit| commit.oid.as_str())
            .collect();
        if !hashes.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(hashes.join("\n")));
        }
    }

    /// A history's diff: the file in the selected commit against its parent.
    fn open_diff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.cursor.filter(|index| self.selected.contains(index)) else {
            return;
        };
        let (Some(commit), Some(revision)) = (self.commits.get(index), self.revisions.get(index))
        else {
            return;
        };
        let Some(path) = self.scope.path().and_then(|path| {
            Some(
                self.git
                    .read(cx)
                    .repos()
                    .get(self.repo)?
                    .repo
                    .absolute(path),
            )
        }) else {
            return;
        };
        let (left, right) = history_sides(commit, revision);
        window.dispatch_action(
            Box::new(git::OpenCompareDiff {
                repo: self.repo,
                path,
                left,
                right,
            }),
            cx,
        );
    }

    // --- Menus ---

    fn secondary_click(
        &mut self,
        index: usize,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        if !self.selected.contains(&index) {
            self.selected.clear();
            self.selected.insert(index);
            self.cursor = Some(index);
            self.anchor = Some(index);
            self.selection_changed(cx);
        }
        let Some(selection) = self.selection() else {
            return;
        };
        let git = self.git.clone();
        let menu = cx.new(|cx| {
            let menu = ContextMenu::new(window, cx);
            crate::log_actions::commit_menu(menu, &selection, git.read(cx))
        });
        self.show_menu(menu, position, window, cx);
    }

    fn show_menu(
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

    // --- Branches pane ---

    /// A click on a branch in the pane: the log shows that branch (again — all).
    fn filter_by_branch(&mut self, branch: Option<String>, cx: &mut Context<Self>) {
        self.set_filter(|filter| filter.revs = branch.into_iter().collect(), cx);
    }

    fn render_branches(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let refs = self.git.read(cx).refs(self.repo);
        let filtered = self.filter.revs.first().cloned();
        let mut rows: Vec<AnyElement> = Vec::new();
        let row = |id: SharedString,
                   label: SharedString,
                   glyph: IconName,
                   color: Hsla,
                   rev: Option<String>,
                   checkout: Option<(String, RefKind)>,
                   cx: &mut Context<Self>| {
            let selected = rev == filtered;
            let repo = self.repo;
            div()
                .id(id)
                .h(px(24.))
                .mx_1()
                .pl(px(18.))
                .pr_2()
                .flex()
                .items_center()
                .gap_1p5()
                .rounded(px(RADIUS_SM))
                .cursor_pointer()
                .when(selected, |row| row.bg(ui.list_selected_inactive))
                .when(!selected, |row| row.hover(|style| style.bg(ui.hover)))
                .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    if event.click_count() == 2 {
                        if let Some((name, kind)) = &checkout {
                            window.dispatch_action(
                                Box::new(git::CheckoutRef {
                                    repo,
                                    name: name.clone(),
                                    kind: *kind,
                                }),
                                cx,
                            );
                        }
                        return;
                    }
                    let branch = if this.filter.revs.first() == rev.as_ref() {
                        None
                    } else {
                        rev.clone()
                    };
                    this.filter_by_branch(branch, cx);
                }))
                .child(icon(glyph, color).size(px(13.)))
                .child(div().min_w_0().truncate().child(label))
                .into_any_element()
        };
        let head_label: SharedString = match refs.current() {
            Some(branch) => format!("HEAD ({})", branch.name).into(),
            None => "HEAD".into(),
        };
        rows.push(row(
            "branch-head".into(),
            head_label,
            IconName::Commit,
            ui.accent,
            Some("HEAD".into()),
            None,
            cx,
        ));
        let sections: [(&'static str, &'static str, RefKind); 3] = [
            ("local", "Local", RefKind::Local),
            ("remote", "Remote", RefKind::Remote),
            ("tags", "Tags", RefKind::Tag),
        ];
        for (key, title, kind) in sections {
            let list = match kind {
                RefKind::Local => &refs.local,
                RefKind::Remote => &refs.remote,
                RefKind::Tag => &refs.tags,
            };
            if list.is_empty() {
                continue;
            }
            let collapsed = self.collapsed.contains(key);
            rows.push(
                div()
                    .id(SharedString::from(format!("branch-section-{key}")))
                    .h(px(24.))
                    .mx_1()
                    .px_1()
                    .flex()
                    .items_center()
                    .gap_1()
                    .rounded(px(RADIUS_SM))
                    .cursor_pointer()
                    .hover(|style| style.bg(ui.hover))
                    .text_color(ui.text_muted)
                    .font_weight(FontWeight::MEDIUM)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if !this.collapsed.remove(key) {
                            this.collapsed.insert(key);
                        }
                        cx.notify();
                    }))
                    .child(
                        icon(
                            if collapsed {
                                IconName::ChevronRight
                            } else {
                                IconName::ChevronDown
                            },
                            ui.dim,
                        )
                        .size(px(12.)),
                    )
                    .child(tr(title))
                    .child(div().text_color(ui.dim).child(list.len().to_string()))
                    .into_any_element(),
            );
            if collapsed {
                continue;
            }
            for reference in list.iter() {
                let (glyph, color) = label_style(kind, reference.current, &ui);
                let checkout = Some((reference.name.clone(), kind));
                rows.push(row(
                    format!("branch-{key}-{}", reference.name).into(),
                    reference.name.clone().into(),
                    if kind == RefKind::Tag {
                        IconName::Tag
                    } else {
                        glyph
                    },
                    color,
                    Some(reference.name.clone()),
                    checkout,
                    cx,
                ));
            }
        }
        div()
            .id("git-log-branches")
            .flex_none()
            .w(px(BRANCHES_WIDTH))
            .h_full()
            .py_1()
            .border_r_1()
            .border_color(ui.divider)
            .overflow_y_scroll()
            .children(rows)
    }

    // --- Rendering ---

    fn render_filter_bar(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let chip = |id: &'static str,
                    label: String,
                    active: bool,
                    menu: Option<FilterMenu>,
                    cx: &mut Context<Self>| {
            div()
                .id(id)
                .flex_none()
                .h(px(24.))
                .px_2()
                .flex()
                .items_center()
                .gap_1()
                .rounded(px(RADIUS_SM))
                .cursor_pointer()
                .text_color(if active {
                    ui.accent_text
                } else {
                    ui.text_muted
                })
                .when(active, |chip| chip.bg(ui.accent_soft))
                .hover(|style| style.bg(ui.hover).text_color(ui.foreground))
                .whitespace_nowrap()
                .child(label)
                .child(icon(IconName::ChevronDown, ui.dim).size(px(10.)))
                .when_some(menu, |chip, menu| {
                    chip.on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.open_filter_menu(menu, event.position, window, cx)
                        }),
                    )
                })
        };
        let git = self.git.read(cx);
        let several_repos = git.repos().len() > 1;
        let repo_name = git.repo_name(self.repo);
        let filter = &self.filter;
        let branch = match filter.revs.first() {
            Some(rev) => trf("Branch: {0}", &[rev]),
            None => tr("Branch").to_string(),
        };
        let user = match filter.authors.as_slice() {
            [] => tr("User").to_string(),
            [one] => trf("User: {0}", &[one]),
            many => trf("User: {0}", &[&many.join(", ")]),
        };
        let date = match filter.since {
            None => tr("Date").to_string(),
            Some(since) => trf("Since {0}", &[&format_day(since + local_offset(since))]),
        };
        let paths = match filter.paths.as_slice() {
            [] => tr("Paths").to_string(),
            paths => trf("Paths: {0}", &[&paths.join(", ")]),
        };
        let all = self.scope == LogScope::All;
        let filtered = *filter != LogFilter::default();
        let bar = div()
            .flex_none()
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .h(px(FILTER_BAR_HEIGHT))
            .px_2()
            .flex()
            .items_center()
            .gap_1()
            .text_size(px(theme::TEXT_SM));
        let bar = if all {
            bar.child(
                ui::toggle_button(
                    "git-log-branches-toggle",
                    IconName::Branch,
                    self.branches_shown(),
                    ui,
                )
                .tooltip(ui::tooltip(tr("Branches"), None))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.branches_open = Some(!this.branches_shown());
                    cx.notify();
                })),
            )
            .child(div().w(px(200.)).flex_none().child(self.search.clone()))
            .when(several_repos, |bar| {
                bar.child(chip(
                    "git-log-repo",
                    repo_name,
                    false,
                    Some(FilterMenu::Repo),
                    cx,
                ))
            })
            .child(chip(
                "git-log-branch",
                branch,
                !filter.revs.is_empty(),
                Some(FilterMenu::Branch),
                cx,
            ))
            .child(chip(
                "git-log-user",
                user,
                !filter.authors.is_empty(),
                Some(FilterMenu::User),
                cx,
            ))
            .child(chip(
                "git-log-date",
                date,
                filter.since.is_some(),
                Some(FilterMenu::Date),
                cx,
            ))
            .child(
                chip("git-log-paths", paths, !filter.paths.is_empty(), None, cx).on_click(
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.edit_filter(FilterKind::Path, window, cx)
                    }),
                ),
            )
        } else {
            let scope = match &self.scope {
                LogScope::Lines { path, start, end } => format!("{path}:{start}–{end}"),
                LogScope::File { path } => path.clone(),
                LogScope::All => String::new(),
            };
            bar.child(icon(IconName::History, ui.dim).size(px(14.)))
                .child(div().ml_1().text_color(ui.text_muted).child(scope))
        };
        let editing = self.editing.as_ref().map(|editing| {
            div()
                .key_context("GitLogFilter")
                .flex_none()
                .w(px(280.))
                .on_action(
                    cx.listener(|this, _: &crate::input_dialog::Confirm, window, cx| {
                        this.apply_editing(window, cx)
                    }),
                )
                .on_action(
                    cx.listener(|this, _: &crate::input_dialog::Dismiss, window, cx| {
                        this.cancel_editing(window, cx)
                    }),
                )
                .child(editing.input.clone())
        });
        let _ = window;
        bar.children(editing)
            .child(div().flex_1())
            .when(self.loading, |bar| {
                bar.child(div().text_color(ui.dim).child(tr("Reading…")))
            })
            .when(all && filtered, |bar| {
                bar.child(
                    ui::icon_button("git-log-reset", IconName::Close, ui)
                        .tooltip(ui::tooltip(tr("Reset Filters"), None))
                        .on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.reset_filters(cx)),
                        ),
                )
            })
            .child(
                ui::icon_button("git-log-refresh", IconName::Refresh, ui)
                    .tooltip(ui::tooltip(tr("Refresh"), None))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.reload(cx))),
            )
    }

    fn render_list(&self, focused: bool, cx: &mut Context<Self>) -> AnyElement {
        let ui = Theme::ui(cx);
        if self.commits.is_empty() {
            let (glyph, text) = if self.git.read(cx).repos().get(self.repo).is_none() {
                (IconName::Branch, tr("Not a Git repository").to_string())
            } else if let Some(error) = &self.error {
                (IconName::Warning, error.to_string())
            } else if self.loading {
                (IconName::History, tr("Reading…").to_string())
            } else if self.filter != LogFilter::default() {
                (
                    IconName::Search,
                    tr("No commits match the filters").to_string(),
                )
            } else {
                (IconName::History, tr("No commits yet").to_string())
            };
            return div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .child(icon(glyph, ui.dim).size(px(20.)))
                .child(div().text_color(ui.text_muted).child(text))
                .into_any_element();
        }
        let now = now_seconds();
        let graph = self.scope == LogScope::All;
        uniform_list(
            "git-log-rows",
            self.commits.len(),
            cx.processor(move |this, range: Range<usize>, _, cx| {
                if range.end + PREFETCH >= this.commits.len() && !this.loading && !this.exhausted {
                    this.load_page(cx);
                }
                range
                    .map(|index| this.render_row(index, graph, focused, now, ui, cx))
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(self.scroll.clone())
        .size_full()
        .into_any_element()
    }

    fn branches_shown(&self) -> bool {
        self.branches_open.unwrap_or(self.wide)
    }

    fn render_row(
        &self,
        index: usize,
        graph: bool,
        focused: bool,
        now: i64,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let commit = &self.commits[index];
        let selected = self.selected.contains(&index);
        let head = self.head.as_deref() == Some(commit.oid.as_str());
        let labels = self.labels.get(&commit.oid);
        let graph_cell = graph
            .then(|| self.graph.row(index))
            .flatten()
            .map(|row| git_graph::render_cell(row, ROW_HEIGHT, head, &ui));
        let revision = self.revisions.get(index);
        let renamed = revision
            .filter(|revision| {
                revision.status == FileStatus::Renamed || revision.orig_path.is_some()
            })
            .map(|revision| revision.path.clone());
        let row = div()
            .id(("git-log-row", index))
            .flex_1()
            .min_w_0()
            .h(px(ROW_HEIGHT))
            .pl_1()
            .pr_2()
            .flex()
            .items_center()
            .gap_2()
            .rounded(px(RADIUS_SM))
            .when(selected, |row| {
                row.bg(if focused {
                    ui.list_selected
                } else {
                    ui.list_selected_inactive
                })
            })
            .when(!selected, |row| row.hover(|style| style.bg(ui.hover)))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.click_row(index, event, window, cx)
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.secondary_click(index, event.position, window, cx)
                }),
            )
            .children(graph_cell)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_1()
                    .overflow_hidden()
                    // At most two labels, then their count: the message keeps its room.
                    .children(
                        labels
                            .into_iter()
                            .flatten()
                            .take(MAX_LABELS)
                            .map(|label| render_label(label, ui).into_any_element()),
                    )
                    .children(
                        labels
                            .map(|labels| labels.len())
                            .filter(|count| *count > MAX_LABELS)
                            .map(|count| {
                                div()
                                    .flex_none()
                                    .text_color(ui.dim)
                                    .text_size(px(theme::TEXT_XS))
                                    .child(format!("+{}", count - MAX_LABELS))
                            }),
                    )
                    .when(
                        head && labels.is_none_or(|labels| !labels.iter().any(|l| l.current)),
                        |row| row.child(ui::badge("HEAD", ui.accent)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(80.))
                            .truncate()
                            .text_color(ui.foreground)
                            .when(head, |text| text.font_weight(FontWeight::MEDIUM))
                            .child(commit.summary.clone()),
                    )
                    .children(renamed.map(|path| div().flex_none().text_color(ui.dim).child(path))),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(AUTHOR_WIDTH))
                    .truncate()
                    .text_color(ui.text_muted)
                    .child(commit.author.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(DATE_WIDTH))
                    .truncate()
                    .text_color(ui.dim)
                    .child(format_time(now, commit.author_time)),
            );
        div().w_full().px_1().flex().child(row).into_any_element()
    }
}

/// Which filter menu is open.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FilterMenu {
    Repo,
    Branch,
    User,
    Date,
}

fn split_revisions(revisions: Vec<FileRevision>) -> (Vec<LogCommit>, Vec<FileRevision>) {
    (
        revisions
            .iter()
            .map(|revision| revision.commit.clone())
            .collect(),
        revisions,
    )
}

/// How many lines a file has in HEAD (to keep `git log -L` within it).
fn lines_in_head(repo: &flux_git::Repo, path: &str) -> Option<u32> {
    let text = repo
        .git()
        .read_only()
        .args(["show", &format!("HEAD:{path}")])
        .output_string()
        .ok()?;
    Some(text.lines().count().max(1) as u32)
}

/// A history's diff sides: the file at the commit's parent (under its old name after a rename)
/// and at the commit.
fn history_sides(commit: &LogCommit, revision: &FileRevision) -> (DiffSide, DiffSide) {
    let short = commit.short().to_string();
    (
        DiffSide::Revision {
            rev: format!("{}^", commit.oid),
            path: revision
                .orig_path
                .clone()
                .unwrap_or_else(|| revision.path.clone()),
            label: format!("{short}^"),
        },
        DiffSide::Revision {
            rev: commit.oid.clone(),
            path: revision.path.clone(),
            label: short,
        },
    )
}

/// A branch / tag label's icon and color: the current branch — the accent, local — green, remote —
/// violet, tags — amber.
fn label_style(kind: RefKind, current: bool, ui: &UiColors) -> (IconName, Hsla) {
    match kind {
        _ if current => (IconName::Branch, ui.accent),
        RefKind::Local => (IconName::Branch, ui.green),
        RefKind::Remote => (IconName::Branch, ui.violet),
        RefKind::Tag => (IconName::Tag, ui.amber),
    }
}

fn render_label(label: &Label, ui: UiColors) -> impl IntoElement {
    let (glyph, color) = label_style(label.kind, label.current, &ui);
    div()
        .flex_shrink()
        .min_w(px(56.))
        .max_w(px(140.))
        .overflow_hidden()
        .h(px(18.))
        .px_1p5()
        .flex()
        .items_center()
        .gap_1()
        .rounded(px(ui::RADIUS_XS))
        .bg(UiColors::tint(color, 0.16))
        .text_color(color)
        .text_size(px(theme::TEXT_XS))
        .when(label.current, |label| {
            label.font_weight(FontWeight::SEMIBOLD)
        })
        .child(icon(glyph, color).size(px(11.)).flex_none())
        .child(div().min_w_0().truncate().child(label.name.clone()))
}

// --- Dates ---

/// The local time zone's offset from UTC at a moment, in seconds.
pub(crate) fn local_offset(time: i64) -> i64 {
    #[cfg(target_os = "macos")]
    {
        use core_foundation_sys::date::CFAbsoluteTime;
        use core_foundation_sys::timezone::{CFTimeZoneCopySystem, CFTimeZoneGetSecondsFromGMT};
        // CFAbsoluteTime counts from 2001-01-01.
        const CF_EPOCH: i64 = 978_307_200;
        // SAFETY: the system time zone is a valid CF object, released after use.
        unsafe {
            let zone = CFTimeZoneCopySystem();
            if zone.is_null() {
                return 0;
            }
            let offset = CFTimeZoneGetSecondsFromGMT(zone, (time - CF_EPOCH) as CFAbsoluteTime);
            core_foundation_sys::base::CFRelease(zone as _);
            offset as i64
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = time;
        0
    }
}

/// Days since 1970-01-01 → (year, month, day) (Howard Hinnant's civil_from_days).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// (year, month, day) → days since 1970-01-01.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// "2025-03-14" of a local time (seconds, already shifted to the local zone).
fn format_day(local: i64) -> String {
    let (y, m, d) = civil(local.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

/// "2025-03-14" → the start of that day as seconds (in the local zone's terms, not shifted).
fn parse_day(text: &str) -> Option<i64> {
    let mut parts = text.split('-');
    let y: i64 = parts.next()?.trim().parse().ok()?;
    let m: u32 = parts.next()?.trim().parse().ok()?;
    let d: u32 = parts.next()?.trim().parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d) * 86_400)
}

/// A commit's date in the log: "Today 14:05", "Yesterday 09:30", "2025-03-14 10:00".
fn format_time(now: i64, time: i64) -> String {
    let local = time + local_offset(time);
    let today = (now + local_offset(now)).div_euclid(86_400);
    let day = local.div_euclid(86_400);
    let minutes = local.rem_euclid(86_400) / 60;
    let clock = format!("{:02}:{:02}", minutes / 60, minutes % 60);
    match today - day {
        0 => trf("Today {0}", &[&clock]),
        1 => trf("Yesterday {0}", &[&clock]),
        _ => format!("{} {clock}", format_day(local)),
    }
}

impl Focusable for GitLogView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for GitLogView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let focused = self.focus_handle.is_focused(window);
        let all = self.scope == LogScope::All;
        self.wide = f32::from(window.viewport_size().width) >= NARROW_WINDOW;
        let page = (f32::from(window.viewport_size().height) / 3. / ROW_HEIGHT).max(5.) as isize;
        let center = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(self.render_filter_bar(window, cx))
            .child(ui::divider(ui).mx(px(ui::GAP)))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .pt_1()
                    .child(self.render_list(focused, cx)),
            );
        div()
            .key_context("GitLog")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_row()
            .font_family(theme::UI_FONT)
            .text_size(px(theme::TEXT_SM))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_cursor(1, false, cx)))
            .on_action(
                cx.listener(|this, _: &SelectPrevious, _, cx| this.move_cursor(-1, false, cx)),
            )
            .on_action(cx.listener(|this, _: &ExtendNext, _, cx| this.move_cursor(1, true, cx)))
            .on_action(
                cx.listener(|this, _: &ExtendPrevious, _, cx| this.move_cursor(-1, true, cx)),
            )
            .on_action(
                cx.listener(move |this, _: &PageDown, _, cx| this.move_cursor(page, false, cx)),
            )
            .on_action(
                cx.listener(move |this, _: &PageUp, _, cx| this.move_cursor(-page, false, cx)),
            )
            .on_action(cx.listener(|this, _: &SelectFirst, _, cx| {
                this.move_cursor(isize::MIN / 2, false, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectLast, _, cx| {
                this.move_cursor(isize::MAX / 2, false, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
                this.selected = (0..this.commits.len()).collect();
                this.selection_changed(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &CopyHash, _, cx| this.copy_hashes(cx)))
            .on_action(cx.listener(|this, _: &OpenDiff, window, cx| this.open_diff(window, cx)))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                if this.scope == LogScope::All {
                    window.focus(&this.search.focus_handle(cx));
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleBranches, _, cx| {
                this.branches_open = Some(!this.branches_shown());
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ResetFilters, _, cx| this.reset_filters(cx)))
            .on_action(cx.listener(|this, _: &Refresh, _, cx| this.reload(cx)))
            .on_action(cx.listener(|this, action: &SetBranchFilter, _, cx| {
                this.filter_by_branch(action.branch.clone(), cx)
            }))
            .on_action(cx.listener(|this, action: &SetUserFilter, _, cx| {
                let user = action.user.clone();
                this.set_filter(|filter| filter.authors = user.into_iter().collect(), cx)
            }))
            .on_action(cx.listener(|this, action: &SetDateFilter, _, cx| {
                let since = action.days.map(|days| now_seconds() - days as i64 * 86_400);
                this.set_filter(|filter| filter.since = since, cx)
            }))
            .on_action(cx.listener(|this, action: &EditFilter, window, cx| {
                this.edit_filter(action.kind, window, cx)
            }))
            .on_action(cx.listener(|this, action: &SetRepo, _, cx| this.set_repo(action.repo, cx)))
            // In a narrow window the branches pane gives its room to the commits.
            .when(all && self.branches_shown(), |root| {
                root.child(self.render_branches(cx))
            })
            .child(center)
            .child(
                div()
                    .flex_none()
                    .w(px(DETAILS_WIDTH))
                    .h_full()
                    .overflow_hidden()
                    .border_l_1()
                    .border_color(ui.divider)
                    .child(self.details.clone()),
            )
            .children(
                self.menu
                    .as_ref()
                    .map(|menu| ContextMenu::overlay(&menu.menu, menu.position)),
            )
    }
}

/// The repository-relative path of a file of the project and its repository.
pub fn relative_in_repo(git: &GitStore, path: &std::path::Path) -> Option<(usize, String)> {
    let repo = git.repo_index(path)?;
    let work_dir: PathBuf = git.repos().get(repo)?.repo.work_dir.clone();
    let relative = path.strip_prefix(&work_dir).ok()?;
    Some((repo, relative.to_string_lossy().replace('\\', "/")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_round_trip() {
        for (y, m, d) in [(1970, 1, 1), (2000, 2, 29), (2025, 12, 31), (2026, 10, 9)] {
            let days = days_from_civil(y, m, d);
            assert_eq!(civil(days), (y, m, d));
        }
        assert_eq!(days_from_civil(1970, 1, 2), 1);
    }

    #[test]
    fn dates_parse_and_format() {
        let day = parse_day("2025-03-14").unwrap();
        assert_eq!(format_day(day), "2025-03-14");
        assert_eq!(parse_day("2025-13-01"), None);
        assert_eq!(parse_day("yesterday"), None);
    }

    #[test]
    fn history_diff_uses_the_old_name_on_the_left() {
        let commit = LogCommit {
            oid: "0123456789abcdef".into(),
            parents: vec!["fedcba".into()],
            summary: "Rename".into(),
            author: "A".into(),
            author_email: "a@x".into(),
            author_time: 0,
            committer: "A".into(),
            commit_time: 0,
        };
        let revision = FileRevision {
            commit: commit.clone(),
            path: "docs/journal.md".into(),
            orig_path: Some("docs/notes.md".into()),
            status: FileStatus::Renamed,
        };
        let (left, right) = history_sides(&commit, &revision);
        assert!(
            matches!(left, DiffSide::Revision { ref path, ref rev, .. } if path == "docs/notes.md" && rev == "0123456789abcdef^")
        );
        assert!(matches!(right, DiffSide::Revision { ref path, .. } if path == "docs/journal.md"));
    }
}
