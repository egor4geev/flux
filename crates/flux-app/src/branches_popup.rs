//! The branches popup (⇧⌘B, a click on the branch chip in the title bar, "Branches…" in ⌃V), as
//! the Git Branches popup of JetBrains IDEs: a search field over a tree of the actions (Update
//! Project, Commit, Push, New Branch, Checkout Tag or Revision), the recent, local and remote
//! branches (by prefix: `feature/…` is a folder; remote branches under their remote) and tags;
//! favorites starred and on top of their section; a branch's actions in a submenu to the right
//! (→ or ↵ opens it, ← closes it). With several repositories — a node per repository.
//!
//! The popup only shows and dispatches: a submenu item or an action row closes the popup, gives
//! focus back to where it was and dispatches its action there; the flows (`branch_dialogs`,
//! `git_sync`, `compare_dialog`) do the work. Typing filters everything (fuzzy, as the palette);
//! Space stars the selected branch (branch names have no spaces); a double click checks it out.

use std::cell::Cell;
use std::collections::{BTreeMap, HashSet};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use flux_git::{Ref, RefKind, Refs, RepoState};
use gpui::{
    Action, AnyElement, App, Bounds, ClickEvent, Context, DismissEvent, Entity, EventEmitter,
    FocusHandle, Focusable, FontWeight, KeyBinding, MouseButton, MouseDownEvent, Pixels, Point,
    Render, ScrollStrategy, SharedString, Subscription, Task, UniformListScrollHandle, Window,
    actions, div, point, prelude::*, px, uniform_list,
};

use crate::git::{self, GitStore};
use crate::i18n::{tr, trf};
use crate::icons::{IconName, folder_icon, icon};
use crate::input::{InputEvent, TextInput};
use crate::picker::highlighted_text;
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, RADIUS_LG, RADIUS_SM};
use crate::workspace::Workspace;

actions!(
    branches_popup,
    [
        SelectNext,
        SelectPrevious,
        SelectNextPage,
        SelectPreviousPage,
        Expand,
        Collapse,
        Confirm,
        Dismiss,
        ToggleFavorite,
    ]
);

/// The popover, its parts and its rows.
const WIDTH: f32 = 440.;
const HEADER_HEIGHT: f32 = 44.;
const FOOTER_HEIGHT: f32 = 34.;
const ROW_HEIGHT: f32 = 28.;
/// Rows are inset from the popover's edges; the list from the header and the footer.
const LIST_INSET: f32 = 6.;
/// How many rows show without scrolling.
const MAX_VISIBLE_ROWS: usize = 14;
const INDENT: f32 = 14.;
const ROW_PADDING: f32 = 6.;
const CHEVRON_WIDTH: f32 = 16.;
/// The submenu: its width, its distance from the popover, its inset (as the context menu).
const SUBMENU_MIN_WIDTH: f32 = 250.;
/// The pointer rests on a branch this long before its actions show, as in JetBrains' popup…
const HOVER_OPEN_DELAY: Duration = Duration::from_millis(150);
/// …and this long on another row before an open submenu gives way: the pointer crosses rows on its
/// way into the submenu.
const HOVER_SWITCH_DELAY: Duration = Duration::from_millis(300);
const SUBMENU_GAP: f32 = 4.;
const SUBMENU_PADDING: f32 = 5.;
/// Where the popover goes under the branch chip.
const CHIP_GAP: f32 = 4.;

pub fn init(cx: &mut App) {
    let popup = Some("BranchesPopup");
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, popup),
        KeyBinding::new("up", SelectPrevious, popup),
        KeyBinding::new("ctrl-n", SelectNext, popup),
        KeyBinding::new("ctrl-p", SelectPrevious, popup),
        KeyBinding::new("pagedown", SelectNextPage, popup),
        KeyBinding::new("pageup", SelectPreviousPage, popup),
        KeyBinding::new("enter", Confirm, popup),
        KeyBinding::new("escape", Dismiss, popup),
        // Branch names can't contain spaces: Space stars the selected branch (as in JetBrains
        // IDEs); on any other row it types a space.
        KeyBinding::new("space", ToggleFavorite, popup),
        // ←/→ open and close the tree's nodes and a branch's actions, as in JetBrains' popups: the
        // search field gives them up (bound deeper than the field's own keys, and later).
        KeyBinding::new("right", Expand, Some("BranchesPopup > TextInput")),
        KeyBinding::new("left", Collapse, Some("BranchesPopup > TextInput")),
    ]);
}

/// Opens the popup (or closes it, when open): under the branch chip when `anchor` is given (a
/// click on it), otherwise where the work is (as the ⌃V menu: under the caret, or in the middle).
pub fn open(
    workspace: &mut Workspace,
    anchor: Option<Point<Pixels>>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let git = workspace.git().clone();
    let anchor = anchor.unwrap_or_else(|| {
        crate::vcs_menu::caret_anchor(workspace, window, cx)
            .unwrap_or_else(|| crate::vcs_menu::window_middle(window, WIDTH))
    });
    let active = workspace.active_path(cx);
    let current_repo = git.read(cx).current_repo(active.as_deref());
    workspace.toggle_modal(window, cx, move |window, cx| {
        BranchesPopup::new(git, current_repo, window, cx)
    });
    // A second ⇧⌘B closed the popup: then there is nothing to place.
    workspace.anchor_modal(anchor);
}

/// The branch chip of the title bar: the branch of the active file's repository (or the
/// project's), the commits to pull ↓ and to push ↑, an operation in progress; a click opens the
/// popup under it.
pub fn branch_chip(
    workspace: &Workspace,
    window: &Window,
    cx: &Context<Workspace>,
) -> Option<AnyElement> {
    let ui = Theme::ui(cx);
    let git = workspace.git().read(cx);
    let active = workspace.active_path(cx);
    let mut label: SharedString = git.branch_label(active.as_deref())?;
    let repo = git.current_repo(active.as_deref());
    let (ahead, behind) = repo
        .and_then(|repo| git.repos().get(repo))
        .map_or((0, 0), |entry| {
            (entry.status.branch.ahead, entry.status.branch.behind)
        });
    let operation = repo.map(|repo| git.operation(repo));
    let state = operation.as_deref().and_then(|operation| {
        Some(match operation.state {
            RepoState::Merging => tr("Merging").to_string(),
            RepoState::Rebasing => {
                // HEAD is detached while a rebase runs: the chip names the branch being rebased.
                if let Some(branch) = &operation.rebase_branch {
                    label = branch.clone().into();
                }
                match operation.step {
                    Some((step, total)) => trf("Rebasing {0}/{1}", &[&step, &total]),
                    None => tr("Rebasing").to_string(),
                }
            }
            RepoState::CherryPicking => tr("Cherry-picking").to_string(),
            RepoState::Reverting => tr("Reverting").to_string(),
            RepoState::Normal | RepoState::Bisecting => return None,
        })
    });
    let bounds = Rc::new(Cell::new(None::<Bounds<Pixels>>));
    let was_open = Rc::new(Cell::new(false));
    let workspace = cx.weak_entity();
    let keys = ui::shortcut_for(&git::Branches, window);
    let counter = |glyph: IconName, count: u32, color| {
        div()
            .flex()
            .items_center()
            .gap(px(1.))
            .text_color(color)
            .child(icon(glyph, color).size(px(10.)))
            .child(count.to_string())
    };
    let chip = div()
        .id("title-branch")
        .flex()
        .items_center()
        .gap_1()
        .h(px(22.))
        .px_2()
        .rounded(px(11.))
        .bg(UiColors::tint(ui.violet, 0.12))
        .text_size(px(theme::TEXT_SM))
        .text_color(ui.violet)
        .cursor_pointer()
        .hover(move |style| style.bg(UiColors::tint(ui.violet, 0.2)))
        .tooltip(ui::tooltip(tr("Branches"), keys))
        // A press while the popup is open only closes it (the popup's own "click outside"): the
        // capture phase sees the popup before it goes.
        .capture_any_mouse_down({
            let (was_open, workspace) = (was_open.clone(), workspace.clone());
            move |_, _, cx| {
                let open = workspace
                    .upgrade()
                    .is_some_and(|workspace| workspace.read(cx).modal_is::<BranchesPopup>());
                was_open.set(open);
            }
        })
        // As a macOS pop-up button: the popup opens on the press.
        .on_mouse_down(MouseButton::Left, {
            let bounds = bounds.clone();
            move |_, window, cx| {
                cx.stop_propagation();
                if was_open.get() {
                    return;
                }
                let anchor = bounds
                    .get()
                    .map(|bounds| point(bounds.left(), bounds.bottom() + px(CHIP_GAP)));
                workspace
                    .update(cx, |workspace, cx| open(workspace, anchor, window, cx))
                    .ok();
            }
        })
        .child(icon(IconName::Branch, ui.violet).size(px(12.)))
        .child(label)
        .when(behind > 0, |chip| {
            chip.child(counter(IconName::ArrowDown, behind, ui.vcs_modified))
        })
        .when(ahead > 0, |chip| {
            chip.child(counter(IconName::ArrowUp, ahead, ui.vcs_added))
        });
    Some(
        div()
            .flex()
            .items_center()
            .gap_1p5()
            // The chip's place on screen, for the popup to open right under it.
            .on_children_prepainted(move |children, _, _| bounds.set(children.first().copied()))
            .child(chip)
            .children(state.map(|state| ui::badge(state, ui.warning)))
            .into_any_element(),
    )
}

/// The popup's window-level actions.
pub fn workspace_actions(root: gpui::Div, cx: &mut Context<Workspace>) -> gpui::Div {
    root.on_action(cx.listener(|this, _: &git::Branches, window, cx| open(this, None, window, cx)))
}

// --- The tree ---

/// A part of a repository's branches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Section {
    Recent,
    Local,
    Remote,
    Tags,
}

impl Section {
    fn label(self) -> &'static str {
        match self {
            Section::Recent => tr("Recent"),
            Section::Local => tr("Local"),
            Section::Remote => tr("Remote"),
            Section::Tags => tr("Tags"),
        }
    }

    /// Tags are many and seldom needed: their node starts collapsed, as in JetBrains IDEs.
    fn expanded_by_default(self) -> bool {
        self != Section::Tags
    }
}

/// An action at the top of the popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TopAction {
    UpdateProject,
    Commit,
    Push,
    NewBranch,
    CheckoutRevision,
}

impl TopAction {
    const ALL: [TopAction; 5] = [
        TopAction::UpdateProject,
        TopAction::Commit,
        TopAction::Push,
        TopAction::NewBranch,
        TopAction::CheckoutRevision,
    ];

    fn label(self) -> &'static str {
        match self {
            TopAction::UpdateProject => tr("Update Project…"),
            TopAction::Commit => tr("Commit…"),
            TopAction::Push => tr("Push…"),
            TopAction::NewBranch => tr("New Branch…"),
            TopAction::CheckoutRevision => tr("Checkout Tag or Revision…"),
        }
    }

    fn icon(self) -> IconName {
        match self {
            TopAction::UpdateProject => IconName::Update,
            TopAction::Commit => IconName::Commit,
            TopAction::Push => IconName::Push,
            TopAction::NewBranch => IconName::Plus,
            TopAction::CheckoutRevision => IconName::Tag,
        }
    }

    fn action(self) -> Box<dyn Action> {
        match self {
            TopAction::UpdateProject => Box::new(git::UpdateProject),
            TopAction::Commit => Box::new(git::Commit),
            TopAction::Push => Box::new(git::Push),
            TopAction::NewBranch => Box::new(git::NewBranch),
            TopAction::CheckoutRevision => Box::new(git::CheckoutRevision),
        }
    }
}

/// A branch or a tag in the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BranchRow {
    pub repo: usize,
    pub section: Section,
    pub kind: RefKind,
    /// "feature/x", "origin/feature/x", "v1.0".
    pub name: String,
    pub current: bool,
    pub favorite: bool,
    /// Commits to push and to pull (a local branch with an upstream).
    pub ahead: u32,
    pub behind: u32,
    pub upstream: Option<String>,
}

impl BranchRow {
    fn of(repo: usize, section: Section, reference: &Ref, favorite: bool) -> Self {
        Self {
            repo,
            section,
            kind: reference.kind,
            name: reference.name.clone(),
            current: reference.current,
            favorite,
            ahead: reference.ahead,
            behind: reference.behind,
            upstream: reference.upstream.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RowKind {
    Action(TopAction),
    /// "New Branch 'query'…": the query is a branch name nothing has yet.
    CreateNamed(String),
    Repo {
        repo: usize,
        branch: Option<String>,
    },
    Section {
        repo: usize,
        section: Section,
        count: usize,
    },
    /// Branches with a common prefix (`feature`), or a remote's branches (`origin`).
    Folder {
        repo: usize,
        section: Section,
        path: String,
    },
    Branch(BranchRow),
    /// A quiet line in place of rows: nothing matches.
    Note(String),
}

/// A visible row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Row {
    pub kind: RowKind,
    pub depth: usize,
    /// What the row shows: a branch's last part inside a folder, its full name otherwise.
    pub label: String,
    /// Matched characters of the label.
    pub positions: Vec<usize>,
    /// How well a branch matches the query (higher is better).
    pub score: u32,
    pub expanded: bool,
}

impl Row {
    fn new(kind: RowKind, depth: usize, label: impl Into<String>) -> Self {
        Self {
            kind,
            depth,
            label: label.into(),
            positions: Vec::new(),
            score: 0,
            expanded: false,
        }
    }

    fn node(&self) -> Option<NodeKey> {
        match &self.kind {
            RowKind::Repo { repo, .. } => Some(NodeKey::Repo(*repo)),
            RowKind::Section { repo, section, .. } => Some(NodeKey::Section(*repo, *section)),
            RowKind::Folder {
                repo,
                section,
                path,
            } => Some(NodeKey::Folder(*repo, *section, path.clone())),
            _ => None,
        }
    }

    fn selectable(&self) -> bool {
        !matches!(self.kind, RowKind::Note(_))
    }

    fn branch(&self) -> Option<&BranchRow> {
        match &self.kind {
            RowKind::Branch(branch) => Some(branch),
            _ => None,
        }
    }
}

/// A node that opens and closes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum NodeKey {
    Repo(usize),
    Section(usize, Section),
    Folder(usize, Section, String),
}

impl NodeKey {
    fn expanded_by_default(&self) -> bool {
        match self {
            NodeKey::Section(_, section) => section.expanded_by_default(),
            NodeKey::Repo(_) | NodeKey::Folder(..) => true,
        }
    }
}

/// What a row is, kept across rebuilds: the selection follows it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RowId {
    Action(TopAction),
    Create,
    Node(NodeKey),
    Branch(usize, Section, String),
}

fn row_id(row: &Row) -> Option<RowId> {
    match &row.kind {
        RowKind::Action(action) => Some(RowId::Action(*action)),
        RowKind::CreateNamed(_) => Some(RowId::Create),
        RowKind::Branch(branch) => Some(RowId::Branch(
            branch.repo,
            branch.section,
            branch.name.clone(),
        )),
        RowKind::Note(_) => None,
        _ => row.node().map(RowId::Node),
    }
}

/// A repository as the popup lists it.
pub(crate) struct RepoInput {
    pub repo: usize,
    pub name: String,
    /// The current branch, or the short hash of a detached HEAD.
    pub branch: Option<String>,
    pub refs: Arc<Refs>,
    pub favorites: HashSet<String>,
}

/// The rows: the actions, then per repository (a node of its own when there are several) Recent,
/// Local, Remote and Tags. Without a query, branches are a tree by prefix (remote ones under their
/// remote) with the favorites flat on top of each section, and nodes open or closed as `toggled`
/// says (it holds the nodes that differ from their default). With a query, everything that matches
/// is listed flat by its full name, best matches first; the actions too.
pub(crate) fn build_rows(repos: &[RepoInput], query: &str, toggled: &HashSet<NodeKey>) -> Vec<Row> {
    let query = query.trim();
    let mut rows = Vec::new();
    let expanded = |key: &NodeKey| key.expanded_by_default() != toggled.contains(key);
    if query.is_empty() {
        rows.extend(
            TopAction::ALL
                .iter()
                .map(|action| Row::new(RowKind::Action(*action), 0, action.label())),
        );
    } else {
        let labels: Vec<&str> = TopAction::ALL.iter().map(|action| action.label()).collect();
        for found in flux_search::match_list(query, &labels) {
            let action = TopAction::ALL[found.index];
            let mut row = Row::new(RowKind::Action(action), 0, action.label());
            row.positions = found.positions;
            rows.push(row);
        }
        // A name nothing has yet: offer to create it, as the first thing after the actions.
        // A tag of that name would make the name ambiguous: not offered either.
        let exists = repos.iter().any(|input| {
            let refs = &input.refs;
            refs.local
                .iter()
                .chain(&refs.remote)
                .chain(&refs.tags)
                .any(|reference| reference.name == query)
        });
        if !exists && !repos.is_empty() && flux_git::check_branch_name(query).is_ok() {
            rows.push(Row::new(
                RowKind::CreateNamed(query.to_string()),
                0,
                trf("New Branch '{0}'…", &[&query]),
            ));
        }
    }
    let several = repos.len() > 1;
    let before_branches = rows.len();
    for input in repos {
        let mut depth = 0;
        let repo_row = rows.len();
        if several {
            let key = NodeKey::Repo(input.repo);
            let open = !query.is_empty() || expanded(&key);
            let mut row = Row::new(
                RowKind::Repo {
                    repo: input.repo,
                    branch: input.branch.clone(),
                },
                0,
                input.name.clone(),
            );
            row.expanded = open;
            rows.push(row);
            if !open {
                continue;
            }
            depth = 1;
        }
        let added_before = rows.len();
        if query.is_empty() {
            push_sections(&mut rows, input, depth, &expanded);
        } else {
            push_matches(&mut rows, input, depth, query);
            // A repository with nothing that matches isn't listed.
            if several && rows.len() == added_before {
                rows.truncate(repo_row);
            }
        }
    }
    if !query.is_empty() && rows.len() == before_branches && rows.is_empty() {
        rows.push(Row::new(
            RowKind::Note(tr("Nothing matches").into()),
            0,
            tr("Nothing matches"),
        ));
    }
    rows
}

/// The sections of a repository without a query.
fn push_sections(
    rows: &mut Vec<Row>,
    input: &RepoInput,
    depth: usize,
    expanded: &dyn Fn(&NodeKey) -> bool,
) {
    let refs = &input.refs;
    let repo = input.repo;
    let favorite = |name: &str| input.favorites.contains(name);
    let recent: Vec<BranchRow> = refs
        .recent
        .iter()
        .filter_map(|name| refs.local(name))
        .filter(|branch| !branch.current)
        .map(|branch| BranchRow::of(repo, Section::Recent, branch, favorite(&branch.name)))
        .collect();
    if !recent.is_empty() {
        let key = NodeKey::Section(repo, Section::Recent);
        let open = expanded(&key);
        push_section_row(rows, repo, Section::Recent, recent.len(), depth, open);
        if open {
            for branch in recent {
                let label = branch.name.clone();
                rows.push(Row::new(RowKind::Branch(branch), depth + 1, label));
            }
        }
    }
    let local: Vec<BranchRow> = refs
        .local
        .iter()
        .map(|branch| BranchRow::of(repo, Section::Local, branch, favorite(&branch.name)))
        .collect();
    if !local.is_empty() {
        let key = NodeKey::Section(repo, Section::Local);
        let open = expanded(&key);
        push_section_row(rows, repo, Section::Local, local.len(), depth, open);
        if open {
            let (favorites, others): (Vec<_>, Vec<_>) =
                local.into_iter().partition(|branch| branch.favorite);
            push_flat(rows, favorites, depth + 1);
            let items = others
                .into_iter()
                .map(|branch| (branch.name.clone(), branch))
                .collect();
            push_tree(rows, repo, Section::Local, "", items, depth + 1, expanded);
        }
    }
    let remote: Vec<BranchRow> = refs
        .remote
        .iter()
        .map(|branch| BranchRow::of(repo, Section::Remote, branch, favorite(&branch.name)))
        .collect();
    if !remote.is_empty() {
        let key = NodeKey::Section(repo, Section::Remote);
        let open = expanded(&key);
        push_section_row(rows, repo, Section::Remote, remote.len(), depth, open);
        if open {
            let (favorites, others): (Vec<_>, Vec<_>) =
                remote.into_iter().partition(|branch| branch.favorite);
            push_flat(rows, favorites, depth + 1);
            // Under their remote: "origin" is a folder, then the prefixes of the branch names.
            let items = others
                .into_iter()
                .map(|branch| (branch.name.clone(), branch))
                .collect();
            push_tree(rows, repo, Section::Remote, "", items, depth + 1, expanded);
        }
    }
    if !refs.tags.is_empty() {
        let key = NodeKey::Section(repo, Section::Tags);
        let open = expanded(&key);
        push_section_row(rows, repo, Section::Tags, refs.tags.len(), depth, open);
        if open {
            let tags = refs
                .tags
                .iter()
                .map(|tag| BranchRow::of(repo, Section::Tags, tag, favorite(&tag.name)))
                .collect();
            push_flat(rows, tags, depth + 1);
        }
    }
}

fn push_section_row(
    rows: &mut Vec<Row>,
    repo: usize,
    section: Section,
    count: usize,
    depth: usize,
    open: bool,
) {
    let mut row = Row::new(
        RowKind::Section {
            repo,
            section,
            count,
        },
        depth,
        section.label(),
    );
    row.expanded = open;
    rows.push(row);
}

/// Branches by their full names, in natural order.
fn push_flat(rows: &mut Vec<Row>, mut branches: Vec<BranchRow>, depth: usize) {
    branches.sort_by(|a, b| flux_fs::compare_names(&a.name, &b.name));
    for branch in branches {
        let label = branch.name.clone();
        rows.push(Row::new(RowKind::Branch(branch), depth, label));
    }
}

/// Folders of a tree by prefix.
#[derive(Default)]
struct Dir {
    dirs: BTreeMap<String, Dir>,
    /// Branches directly inside: their last part, and the branch.
    leaves: Vec<(String, BranchRow)>,
}

/// Branches as a tree by `/`: folders first (natural order), then the branches of the level.
/// `items` are (name relative to `prefix`, branch).
fn push_tree(
    rows: &mut Vec<Row>,
    repo: usize,
    section: Section,
    prefix: &str,
    items: Vec<(String, BranchRow)>,
    depth: usize,
    expanded: &dyn Fn(&NodeKey) -> bool,
) {
    let mut root = Dir::default();
    for (name, branch) in items {
        let parts: Vec<&str> = name.split('/').collect();
        let mut node = &mut root;
        for part in &parts[..parts.len() - 1] {
            node = node.dirs.entry(part.to_string()).or_default();
        }
        node.leaves
            .push((parts[parts.len() - 1].to_string(), branch));
    }
    emit_dir(rows, repo, section, prefix, root, depth, expanded);
}

fn emit_dir(
    rows: &mut Vec<Row>,
    repo: usize,
    section: Section,
    prefix: &str,
    dir: Dir,
    depth: usize,
    expanded: &dyn Fn(&NodeKey) -> bool,
) {
    let mut dirs: Vec<(String, Dir)> = dir.dirs.into_iter().collect();
    dirs.sort_by(|a, b| flux_fs::compare_names(&a.0, &b.0));
    for (name, child) in dirs {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let key = NodeKey::Folder(repo, section, path.clone());
        let open = expanded(&key);
        let mut row = Row::new(
            RowKind::Folder {
                repo,
                section,
                path: path.clone(),
            },
            depth,
            name,
        );
        row.expanded = open;
        rows.push(row);
        if open {
            emit_dir(rows, repo, section, &path, child, depth + 1, expanded);
        }
    }
    let mut leaves = dir.leaves;
    leaves.sort_by(|a, b| flux_fs::compare_names(&a.0, &b.0));
    for (label, branch) in leaves {
        rows.push(Row::new(RowKind::Branch(branch), depth, label));
    }
}

/// The branches of a repository that match a query: per section, best first, full names.
fn push_matches(rows: &mut Vec<Row>, input: &RepoInput, depth: usize, query: &str) {
    let refs = &input.refs;
    let favorite = |name: &str| input.favorites.contains(name);
    for (section, list) in [
        (Section::Local, &refs.local),
        (Section::Remote, &refs.remote),
        (Section::Tags, &refs.tags),
    ] {
        let names: Vec<&str> = list
            .iter()
            .map(|reference| reference.name.as_str())
            .collect();
        let found = flux_search::match_list(query, &names);
        if found.is_empty() {
            continue;
        }
        push_section_row(rows, input.repo, section, found.len(), depth, true);
        for found in found {
            let reference = &list[found.index];
            let branch = BranchRow::of(input.repo, section, reference, favorite(&reference.name));
            let mut row = Row::new(RowKind::Branch(branch), depth + 1, reference.name.clone());
            row.positions = found.positions;
            row.score = found.score;
            rows.push(row);
        }
    }
}

// --- A branch's actions ---

/// An item of a branch's submenu.
pub(crate) struct SubItem {
    pub label: String,
    pub action: Box<dyn Action>,
}

/// The actions of a branch or a tag, in JetBrains' order (`None` — a separator). `current` is the
/// current branch's name, or the short hash of a detached HEAD.
pub(crate) fn submenu_items(branch: &BranchRow, current: &str) -> Vec<Option<SubItem>> {
    let repo = branch.repo;
    let name = branch.name.clone();
    let item = |label: String, action: Box<dyn Action>| Some(SubItem { label, action });
    let checkout = item(
        tr("Checkout").into(),
        Box::new(git::CheckoutRef {
            repo,
            name: name.clone(),
            kind: branch.kind,
        }),
    );
    let new_branch = item(
        trf("New Branch from '{0}'…", &[&name]),
        Box::new(git::NewBranchFrom {
            repo,
            start: name.clone(),
        }),
    );
    let checkout_rebase = item(
        trf("Checkout and Rebase onto '{0}'", &[&current]),
        Box::new(git::CheckoutAndRebase {
            repo,
            branch: name.clone(),
            onto: current.to_string(),
        }),
    );
    let compare = item(
        trf("Compare with '{0}'", &[&current]),
        Box::new(git::CompareWithCurrent {
            repo,
            name: name.clone(),
        }),
    );
    let diff = item(
        tr("Show Diff with Working Tree").into(),
        Box::new(git::DiffWithWorkingTree {
            repo,
            name: name.clone(),
        }),
    );
    let rebase = item(
        trf("Rebase '{0}' onto '{1}'", &[&current, &name]),
        Box::new(git::RebaseOnto {
            repo,
            onto: name.clone(),
        }),
    );
    let merge = item(
        trf("Merge '{0}' into '{1}'", &[&name, &current]),
        Box::new(git::MergeRef {
            repo,
            name: name.clone(),
        }),
    );
    let update = branch.upstream.is_some().then(|| {
        item(
            tr("Update").into(),
            Box::new(git::UpdateBranch {
                repo,
                name: name.clone(),
            }),
        )
    });
    let push = item(
        tr("Push…").into(),
        Box::new(git::PushBranch {
            repo,
            name: name.clone(),
        }),
    );
    let rename = item(
        tr("Rename…").into(),
        Box::new(git::RenameBranch {
            repo,
            name: name.clone(),
        }),
    );
    let delete = item(
        tr("Delete").into(),
        Box::new(git::DeleteRef {
            repo,
            name: name.clone(),
            kind: branch.kind,
        }),
    );
    let mut items = Vec::new();
    match branch.kind {
        RefKind::Local if branch.current => {
            items.push(new_branch);
            items.push(None);
            items.extend(update);
            items.push(push);
            items.push(None);
            items.push(rename);
        }
        RefKind::Local => {
            items.extend([
                checkout,
                new_branch,
                checkout_rebase,
                None,
                compare,
                diff,
                None,
            ]);
            items.extend([rebase, merge, None]);
            items.extend(update);
            items.extend([push, None, rename, delete]);
        }
        RefKind::Remote => {
            let pull = |rebase: bool, label: String| {
                item(
                    label,
                    Box::new(git::PullRef {
                        repo,
                        name: name.clone(),
                        rebase,
                    }),
                )
            };
            items.extend([
                checkout,
                new_branch,
                checkout_rebase,
                None,
                compare,
                diff,
                None,
            ]);
            items.extend([
                rebase,
                merge,
                pull(true, trf("Pull into '{0}' Using Rebase", &[&current])),
                pull(false, trf("Pull into '{0}' Using Merge", &[&current])),
                None,
                delete,
            ]);
        }
        RefKind::Tag => {
            items.extend([checkout, new_branch, None, compare, diff, None, merge, None]);
            items.push(delete);
        }
    }
    // No separator twice, first or last (an item that didn't apply leaves two side by side).
    let mut cleaned: Vec<Option<SubItem>> = Vec::new();
    for item in items {
        if item.is_none() && cleaned.last().is_none_or(Option::is_none) {
            continue;
        }
        cleaned.push(item);
    }
    while cleaned.last().is_some_and(Option::is_none) {
        cleaned.pop();
    }
    cleaned
}

/// The open submenu of a branch row.
struct Submenu {
    /// The row it belongs to.
    row: usize,
    items: Vec<Option<SubItem>>,
    /// The item chosen with the keys.
    selected: Option<usize>,
}

impl Submenu {
    fn enabled(&self) -> impl Iterator<Item = usize> + '_ {
        self.items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.is_some())
            .map(|(index, _)| index)
    }
}

// --- The popup ---

pub struct BranchesPopup {
    git: Entity<GitStore>,
    query: Entity<TextInput>,
    rows: Vec<Row>,
    selected: usize,
    /// Nodes opened or closed against their default.
    toggled: HashSet<NodeKey>,
    submenu: Option<Submenu>,
    /// The row under the pointer, and what resting on it will do (open its submenu, close another).
    hovered: Option<RowId>,
    hover_task: Option<Task<()>>,
    scroll: UniformListScrollHandle,
    /// Where focus was before the popup: an action is dispatched there, keys are looked up there.
    previous_focus: Option<FocusHandle>,
    /// The repository the window works with now.
    current_repo: Option<usize>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DismissEvent> for BranchesPopup {}

impl BranchesPopup {
    fn new(
        git: Entity<GitStore>,
        current_repo: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let previous_focus = window.focused(cx);
        let query =
            cx.new(|cx| TextInput::new(tr("Search for branches and actions"), cx).borderless());
        let subscriptions = vec![
            cx.subscribe_in(&query, window, |this, _, event, _, cx| match event {
                InputEvent::Changed => this.query_changed(cx),
            }),
            // Refs come and go (a fetch, a checkout): the rows follow, the selection stays on its
            // row.
            cx.observe(&git, |this, _, cx| this.rebuild(cx)),
        ];
        git.update(cx, |git, cx| {
            for repo in 0..git.repos().len() {
                git.reload_refs(repo, cx);
            }
        });
        let mut popup = Self {
            git,
            query,
            rows: Vec::new(),
            selected: 0,
            toggled: HashSet::new(),
            submenu: None,
            hovered: None,
            hover_task: None,
            scroll: UniformListScrollHandle::new(),
            previous_focus,
            current_repo,
            _subscriptions: subscriptions,
        };
        popup.rebuild(cx);
        popup.select_initial(cx);
        popup
    }

    fn inputs(&self, cx: &App) -> Vec<RepoInput> {
        let git = self.git.read(cx);
        git.repos()
            .iter()
            .enumerate()
            .map(|(repo, entry)| {
                let refs = entry.refs.clone();
                let favorites = refs
                    .local
                    .iter()
                    .chain(&refs.remote)
                    .chain(&refs.tags)
                    .filter(|reference| git.is_favorite(repo, &reference.name))
                    .map(|reference| reference.name.clone())
                    .collect();
                RepoInput {
                    repo,
                    name: git.repo_name(repo),
                    branch: entry.status.branch.label(),
                    refs,
                    favorites,
                }
            })
            .collect()
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let selected = self.rows.get(self.selected).and_then(row_id);
        let submenu = self
            .submenu
            .as_ref()
            .and_then(|submenu| self.rows.get(submenu.row))
            .and_then(row_id);
        let query = self.query.read(cx).text();
        let inputs = self.inputs(cx);
        self.rows = build_rows(&inputs, &query, &self.toggled);
        if let Some(selected) = selected
            && let Some(index) = self
                .rows
                .iter()
                .position(|row| row_id(row).as_ref() == Some(&selected))
        {
            self.selected = index;
        }
        // The submenu stays with its branch; a branch that is gone takes its submenu along.
        match submenu.and_then(|id| {
            self.rows
                .iter()
                .position(|row| row_id(row).as_ref() == Some(&id))
        }) {
            Some(row) => {
                if let Some(submenu) = &mut self.submenu {
                    submenu.row = row;
                }
            }
            None => self.submenu = None,
        }
        self.clamp_selection();
        cx.notify();
    }

    fn query_changed(&mut self, cx: &mut Context<Self>) {
        self.submenu = None;
        self.rebuild(cx);
        self.select_initial(cx);
        self.scroll
            .scroll_to_item(self.selected, ScrollStrategy::Top);
        cx.notify();
    }

    /// The row to start from: with a query — the best branch, else "New Branch 'query'…" (actions
    /// are found by name too, but a query is mostly a branch); without one — the first action, as in
    /// JetBrains' popup.
    fn select_initial(&mut self, cx: &App) {
        let first = self.rows.iter().position(Row::selectable).unwrap_or(0);
        if self.query.read(cx).text().trim().is_empty() {
            self.selected = first;
            return;
        }
        // The best branch of all the repositories (on a tie, the first one listed).
        let best = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.branch().is_some())
            .max_by(|(a, x), (b, y)| x.score.cmp(&y.score).then(b.cmp(a)))
            .map(|(index, _)| index);
        self.selected = best
            .or_else(|| {
                self.rows
                    .iter()
                    .position(|row| matches!(row.kind, RowKind::CreateNamed(_)))
            })
            .unwrap_or(first);
    }

    fn clamp_selection(&mut self) {
        if self.rows.is_empty() {
            self.selected = 0;
            return;
        }
        self.selected = self.selected.min(self.rows.len() - 1);
        if !self.rows[self.selected].selectable() {
            self.selected = self.rows.iter().position(Row::selectable).unwrap_or(0);
        }
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let index = index.min(self.rows.len() - 1);
        let strategy = if index < self.selected {
            ScrollStrategy::Top
        } else {
            ScrollStrategy::Bottom
        };
        self.selected = index;
        self.scroll.scroll_to_item(index, strategy);
        cx.notify();
    }

    /// The next selectable row in a direction, wrapping around; `page` rows at once.
    fn step(&mut self, forward: bool, page: bool, cx: &mut Context<Self>) {
        if let Some(submenu) = &mut self.submenu {
            let enabled: Vec<usize> = submenu.enabled().collect();
            if enabled.is_empty() {
                return;
            }
            let at = submenu
                .selected
                .and_then(|selected| enabled.iter().position(|index| *index == selected));
            let next = match (at, forward) {
                (None, true) => 0,
                (None, false) => enabled.len() - 1,
                (Some(at), true) => (at + 1) % enabled.len(),
                (Some(at), false) => (at + enabled.len() - 1) % enabled.len(),
            };
            submenu.selected = Some(enabled[next]);
            return cx.notify();
        }
        let selectable: Vec<usize> = (0..self.rows.len())
            .filter(|&index| self.rows[index].selectable())
            .collect();
        if selectable.is_empty() {
            return;
        }
        let at = selectable
            .iter()
            .position(|index| *index == self.selected)
            .unwrap_or(0);
        let next = if page {
            let jump = MAX_VISIBLE_ROWS - 1;
            if forward {
                (at + jump).min(selectable.len() - 1)
            } else {
                at.saturating_sub(jump)
            }
        } else if forward {
            (at + 1) % selectable.len()
        } else {
            (at + selectable.len() - 1) % selectable.len()
        };
        self.select(selectable[next], cx);
    }

    fn set_expanded(&mut self, key: NodeKey, expanded: bool, cx: &mut Context<Self>) {
        if expanded == key.expanded_by_default() {
            self.toggled.remove(&key);
        } else {
            self.toggled.insert(key);
        }
        self.rebuild(cx);
    }

    /// →: a branch's actions; a closed node opens.
    fn expand(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.submenu.is_some() {
            return;
        }
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        if row.branch().is_some() {
            return self.open_submenu(self.selected, true, window, cx);
        }
        if let Some(key) = row.node()
            && !row.expanded
            && self.query.read(cx).is_empty()
        {
            self.set_expanded(key, true, cx);
        }
    }

    /// ←: the submenu closes; an open node closes; inside a node — to the node.
    fn collapse(&mut self, cx: &mut Context<Self>) {
        if self.submenu.take().is_some() {
            return cx.notify();
        }
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        if let Some(key) = row.node()
            && row.expanded
            && self.query.read(cx).is_empty()
        {
            return self.set_expanded(key, false, cx);
        }
        let depth = row.depth;
        if depth == 0 {
            return;
        }
        if let Some(parent) = (0..self.selected)
            .rev()
            .find(|&index| self.rows[index].depth < depth && self.rows[index].node().is_some())
        {
            self.select(parent, cx);
        }
    }

    /// ↵: a submenu item runs; an action runs; a branch shows its actions; a node opens or closes.
    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(submenu) = &self.submenu {
            if let Some(index) = submenu.selected {
                self.run_item(index, window, cx);
            }
            return;
        }
        let Some(row) = self.rows.get(self.selected).cloned() else {
            return;
        };
        match &row.kind {
            RowKind::Action(action) => self.run(action.action(), window, cx),
            RowKind::CreateNamed(name) => self.create_named(name.clone(), window, cx),
            RowKind::Branch(_) => self.open_submenu(self.selected, true, window, cx),
            _ => {
                if let Some(key) = row.node()
                    && self.query.read(cx).is_empty()
                {
                    self.set_expanded(key, !row.expanded, cx);
                }
            }
        }
    }

    /// "New Branch 'query'…": the New Branch dialog with the name filled in, from the current
    /// branch of the window's repository.
    fn create_named(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repo) = self.current_repo else {
            return;
        };
        let git = self.git.read(cx);
        let start = git
            .current_branch(repo)
            .unwrap_or_else(|| "HEAD".to_string());
        self.run(
            Box::new(crate::branch_dialogs::NewBranchNamed { repo, start, name }),
            window,
            cx,
        );
    }

    /// Space: the selected branch gets or loses its star; on another row, Space types a space.
    fn toggle_favorite(&mut self, cx: &mut Context<Self>) {
        let branch = self
            .rows
            .get(self.selected)
            .and_then(Row::branch)
            .filter(|_| self.submenu.is_none())
            .cloned();
        let Some(branch) = branch else {
            return cx.propagate();
        };
        self.git.update(cx, |git, cx| {
            git.toggle_favorite(branch.repo, &branch.name, cx)
        });
        self.rebuild(cx);
    }

    fn current_label(&self, repo: usize, cx: &App) -> String {
        let git = self.git.read(cx);
        git.repos()
            .get(repo)
            .and_then(|entry| entry.status.branch.label())
            .unwrap_or_else(|| "HEAD".to_string())
    }

    fn open_submenu(
        &mut self,
        index: usize,
        select_first: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_submenu(index, select_first, cx);
    }

    /// The pointer came onto a row (`hovered`) or left it. A branch shows its actions after a
    /// short rest, as in JetBrains' popup; with a submenu open, another row takes over only after a
    /// longer one — the pointer may just be crossing it on the way into the submenu.
    fn hover_row(&mut self, index: usize, hovered: bool, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index) else {
            return;
        };
        let id = row_id(row);
        if !hovered {
            if id.is_some() && self.hovered == id {
                self.hovered = None;
            }
            return;
        }
        if !row.selectable() {
            return;
        }
        self.hovered = id.clone();
        if self
            .submenu
            .as_ref()
            .is_some_and(|submenu| submenu.row == index)
        {
            // Back on the branch whose actions are open: nothing changes.
            self.hover_task = None;
            return;
        }
        let is_branch = row.branch().is_some();
        let delay = if self.submenu.is_some() {
            HOVER_SWITCH_DELAY
        } else {
            // Without a submenu the selection follows the pointer right away.
            self.selected = index;
            cx.notify();
            if !is_branch {
                self.hover_task = None;
                return;
            }
            HOVER_OPEN_DELAY
        };
        self.hover_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            this.update(cx, |this, cx| {
                // Still resting on that row (found again: the list may have been rebuilt).
                let Some(index) = id
                    .as_ref()
                    .filter(|id| this.hovered.as_ref() == Some(*id))
                    .and_then(|id| {
                        this.rows
                            .iter()
                            .position(|row| row_id(row).as_ref() == Some(id))
                    })
                else {
                    return;
                };
                this.hover_task = None;
                if this.rows[index].branch().is_some() {
                    this.show_submenu(index, false, cx);
                } else {
                    this.submenu = None;
                    this.selected = index;
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// The pointer reached the submenu: a switch it started on the way is called off.
    fn hover_submenu(&mut self, hovered: bool) {
        if hovered {
            self.hover_task = None;
            self.hovered = None;
        }
    }

    fn show_submenu(&mut self, index: usize, select_first: bool, cx: &mut Context<Self>) {
        let Some(branch) = self.rows.get(index).and_then(Row::branch) else {
            return;
        };
        let current = self.current_label(branch.repo, cx);
        let items = submenu_items(branch, &current);
        let selected = select_first
            .then(|| items.iter().position(Option::is_some))
            .flatten();
        self.selected = index;
        self.submenu = Some(Submenu {
            row: index,
            items,
            selected,
        });
        cx.notify();
    }

    fn run_item(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let action = self
            .submenu
            .as_ref()
            .and_then(|submenu| submenu.items.get(index))
            .and_then(|item| item.as_ref())
            .map(|item| item.action.boxed_clone());
        if let Some(action) = action {
            self.run(action, window, cx);
        }
    }

    /// Runs an action: the popup closes, focus goes back to where it was, and the action is
    /// dispatched there (the window handles it).
    fn run(&mut self, action: Box<dyn Action>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(focus) = &self.previous_focus {
            window.focus(focus);
        }
        cx.emit(DismissEvent);
        window.dispatch_action(action, cx);
    }

    fn click_row(
        &mut self,
        index: usize,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.rows.get(index).cloned() else {
            return;
        };
        if !row.selectable() {
            return;
        }
        self.selected = index;
        match &row.kind {
            RowKind::Action(action) => self.run(action.action(), window, cx),
            RowKind::CreateNamed(name) => self.create_named(name.clone(), window, cx),
            RowKind::Branch(branch) => {
                // A double click checks the branch out, as JetBrains' popup does.
                if event.click_count() >= 2 && !branch.current {
                    let action = git::CheckoutRef {
                        repo: branch.repo,
                        name: branch.name.clone(),
                        kind: branch.kind,
                    };
                    return self.run(Box::new(action), window, cx);
                }
                // A click shows the actions at once (hovering does it after a pause); a click on
                // the branch whose actions are open keeps them.
                self.hover_task = None;
                if self
                    .submenu
                    .as_ref()
                    .is_none_or(|submenu| submenu.row != index)
                {
                    self.open_submenu(index, false, window, cx);
                }
            }
            _ => {
                self.submenu = None;
                if let Some(key) = row.node()
                    && self.query.read(cx).is_empty()
                {
                    self.set_expanded(key, !row.expanded, cx);
                }
                cx.notify();
            }
        }
    }

    // --- Rendering ---

    fn render_row(&self, index: usize, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let ui = Theme::ui(cx);
        let Some(row) = self.rows.get(index) else {
            return div().into_any_element();
        };
        let selected = index == self.selected && row.selectable();
        let submenu_here = self
            .submenu
            .as_ref()
            .is_some_and(|submenu| submenu.row == index);
        let chevron = |expanded: bool| {
            div()
                .flex_none()
                .w(px(CHEVRON_WIDTH))
                .flex()
                .justify_center()
                .child(
                    icon(
                        if expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        },
                        ui.dim,
                    )
                    .size(px(12.)),
                )
        };
        let spacer = || div().flex_none().w(px(CHEVRON_WIDTH));
        let label = |row: &Row, color| {
            div()
                .min_w_0()
                .truncate()
                .text_color(color)
                .child(highlighted_text(
                    row.label.clone(),
                    &row.positions,
                    ui.match_text,
                ))
        };
        let content: AnyElement = match &row.kind {
            RowKind::Action(action) => {
                let keys = self.keys_for(action.action().as_ref(), window);
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .flex_1()
                    .min_w_0()
                    .child(icon(action.icon(), ui.text_muted).size(px(14.)))
                    .child(label(row, ui.foreground).flex_1())
                    .children(keys.map(|keys| ui::keys(&keys, ui)))
                    .into_any_element()
            }
            RowKind::CreateNamed(_) => div()
                .flex()
                .items_center()
                .gap_2()
                .flex_1()
                .min_w_0()
                .child(icon(IconName::Plus, ui.accent_text).size(px(14.)))
                .child(label(row, ui.accent_text))
                .into_any_element(),
            RowKind::Repo { branch, .. } => div()
                .flex()
                .items_center()
                .gap_1p5()
                .flex_1()
                .min_w_0()
                .child(chevron(row.expanded))
                .child(folder_icon(row.expanded, &ui).render().size(px(14.)))
                .child(label(row, ui.foreground).font_weight(FontWeight::SEMIBOLD))
                .children(branch.clone().map(|branch| {
                    div()
                        .flex_none()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.violet)
                        .child(branch)
                }))
                .into_any_element(),
            RowKind::Section { count, .. } => div()
                .flex()
                .items_center()
                .gap_1p5()
                .flex_1()
                .min_w_0()
                .child(chevron(row.expanded))
                .child(ui::section_label(row.label.clone(), ui))
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(ui.dim)
                        .child(count.to_string()),
                )
                .into_any_element(),
            RowKind::Folder { .. } => div()
                .flex()
                .items_center()
                .gap_1p5()
                .flex_1()
                .min_w_0()
                .child(chevron(row.expanded))
                .child(folder_icon(row.expanded, &ui).render().size(px(14.)))
                .child(label(row, ui.foreground))
                .into_any_element(),
            RowKind::Branch(branch) => {
                let star = self.render_star(index, branch, ui, cx);
                let name_color = if branch.current {
                    ui.accent_text
                } else {
                    ui.foreground
                };
                let counter = |glyph: IconName, count: u32, color| {
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(1.))
                        .text_size(px(theme::TEXT_SM))
                        .text_color(color)
                        .child(icon(glyph, color).size(px(10.)))
                        .child(count.to_string())
                };
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .flex_1()
                    .min_w_0()
                    .child(spacer())
                    .child(star)
                    .child(label(row, name_color).when(branch.current, |name| {
                        name.font_weight(FontWeight::SEMIBOLD)
                    }))
                    .child(div().flex_1())
                    .when(branch.behind > 0, |row| {
                        row.child(counter(IconName::ArrowDown, branch.behind, ui.vcs_modified))
                    })
                    .when(branch.ahead > 0, |row| {
                        row.child(counter(IconName::ArrowUp, branch.ahead, ui.vcs_added))
                    })
                    .child(
                        icon(
                            IconName::ChevronRight,
                            if submenu_here || selected {
                                ui.text_muted
                            } else {
                                ui.dim
                            },
                        )
                        .size(px(12.)),
                    )
                    .into_any_element()
            }
            RowKind::Note(text) => div()
                .flex_1()
                .text_color(ui.dim)
                .child(text.clone())
                .into_any_element(),
        };
        div()
            .w_full()
            .h(px(ROW_HEIGHT))
            .px(px(LIST_INSET))
            .child(
                div()
                    .id(("branch-row", index))
                    .size_full()
                    .flex()
                    .items_center()
                    .pl(px(ROW_PADDING + row.depth as f32 * INDENT))
                    .pr_2()
                    .rounded(px(RADIUS_SM))
                    .whitespace_nowrap()
                    .when(row.selectable(), |inner| inner.cursor_pointer())
                    .map(|inner| {
                        if selected || submenu_here {
                            inner.bg(ui.list_selected)
                        } else if row.selectable() {
                            inner.hover(move |style| style.bg(ui.hover))
                        } else {
                            inner
                        }
                    })
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        this.hover_row(index, *hovered, cx)
                    }))
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        this.click_row(index, event, window, cx)
                    }))
                    .child(content),
            )
            .into_any_element()
    }

    /// A branch's star: filled for a favorite; otherwise the branch (or tag) glyph, which a click
    /// turns into a favorite.
    fn render_star(
        &self,
        index: usize,
        branch: &BranchRow,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let (glyph, color) = match (branch.favorite, branch.kind) {
            (true, _) => (IconName::StarFilled, ui.amber),
            (false, RefKind::Tag) => (IconName::Tag, ui.dim),
            (false, _) => (IconName::Branch, ui.dim),
        };
        let tooltip = if branch.favorite {
            tr("Remove from Favorites")
        } else {
            tr("Add to Favorites")
        };
        let (repo, name) = (branch.repo, branch.name.clone());
        div()
            .id(("branch-star", index))
            .flex_none()
            .size(px(18.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(ui::RADIUS_XS))
            .hover(move |style| style.bg(ui.hover))
            .tooltip(ui::tooltip(tooltip, None))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                this.git
                    .update(cx, |git, cx| git.toggle_favorite(repo, &name, cx));
                this.rebuild(cx);
            }))
            .child(icon(glyph, color).size(px(13.)))
    }

    /// The keys of the footer, for what is selected.
    fn hints(&self) -> Vec<(&'static str, &'static str)> {
        if self.submenu.is_some() {
            return vec![("↵", tr("run")), ("←", tr("back")), ("esc", tr("close"))];
        }
        match self.rows.get(self.selected) {
            Some(row) if row.branch().is_some() => vec![
                ("→", tr("actions")),
                ("␣", tr("favorite")),
                ("esc", tr("close")),
            ],
            _ => vec![
                ("↑↓", tr("navigate")),
                ("↵", tr("open")),
                ("esc", tr("close")),
            ],
        }
    }

    /// An action's shortcut where the popup was opened from.
    fn keys_for(&self, action: &dyn Action, window: &Window) -> Option<SharedString> {
        match &self.previous_focus {
            Some(focus) => ui::shortcut_in(action, focus, window),
            None => ui::shortcut_for(action, window),
        }
    }

    fn render_submenu(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let submenu = self.submenu.as_ref()?;
        let ui = Theme::ui(cx);
        // Level with the row: its place in the list, scrolled.
        let scroll = self.scroll.0.borrow().base_handle.offset().y;
        let row_top = HEADER_HEIGHT + 1. + LIST_INSET + submenu.row as f32 * ROW_HEIGHT;
        let top = (px(row_top - SUBMENU_PADDING) + scroll).max(px(0.));
        let last = submenu.items.len().saturating_sub(1);
        let items: Vec<AnyElement> = submenu
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| match item {
                None if index == last => div().into_any_element(),
                None => ui::divider(ui).mx_1p5().my_1().into_any_element(),
                Some(item) => {
                    let selected = submenu.selected == Some(index);
                    div()
                        .id(("branch-action", index))
                        .h(px(ROW_HEIGHT))
                        .px_2p5()
                        .flex()
                        .items_center()
                        .rounded(px(RADIUS_SM))
                        .whitespace_nowrap()
                        .cursor_pointer()
                        .when(selected, |row| row.bg(ui.list_selected))
                        .when(!selected, |row| row.hover(move |style| style.bg(ui.hover)))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.run_item(index, window, cx)
                        }))
                        .child(item.label.clone())
                        .into_any_element()
                }
            })
            .collect();
        Some(
            div().mt(top).child(
                div()
                    .id("branch-submenu")
                    .on_hover(
                        cx.listener(|this, hovered: &bool, _, _| this.hover_submenu(*hovered)),
                    )
                    .relative()
                    .min_w(px(SUBMENU_MIN_WIDTH))
                    .p(px(SUBMENU_PADDING))
                    .flex()
                    .flex_col()
                    .rounded(px(RADIUS_LG))
                    .bg(ui.elevated)
                    .border_1()
                    .border_color(ui.elevated_border)
                    .shadow(ui::popover_shadow(ui))
                    // A click inside doesn't reach the list behind it.
                    .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _, cx| {
                        cx.stop_propagation()
                    })
                    .child(ui::sheen(ui, RADIUS_LG))
                    .children(items),
            ),
        )
    }

    fn render_empty(&self, cx: &App) -> Option<impl IntoElement + use<>> {
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
            return None;
        };
        Some(
            div()
                .h(px(ROW_HEIGHT * 3.))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_color(ui.text_muted)
                        .child(icon(glyph, ui.dim).size(px(14.)))
                        .child(title),
                )
                .children(hint.map(|hint| {
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.dim)
                        .child(hint)
                })),
        )
    }
}

impl Focusable for BranchesPopup {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.focus_handle(cx)
    }
}

impl Render for BranchesPopup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        self.clamp_selection();
        let count = self.rows.len();
        let list = match self.render_empty(cx) {
            Some(empty) => empty.into_any_element(),
            None => {
                let height = ROW_HEIGHT * count.clamp(1, MAX_VISIBLE_ROWS) as f32;
                uniform_list(
                    "branch-rows",
                    count,
                    cx.processor(|this, range: Range<usize>, window, cx| {
                        range
                            .map(|index| this.render_row(index, window, cx))
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(self.scroll.clone())
                .h(px(height))
                .into_any_element()
            }
        };
        let activity = self.git.read(cx).activity().cloned();
        let fetch_keys = ui::shortcut_for(&git::Fetch, window);
        let popover = ui::popover(ui)
            .w(px(WIDTH))
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .h(px(HEADER_HEIGHT))
                    .pl_4()
                    .pr_2()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(icon(IconName::Search, ui.text_muted))
                    .child(div().flex_1().min_w_0().child(self.query.clone()))
                    .child(
                        ui::icon_button("branches-fetch", IconName::Refresh, ui)
                            .tooltip(ui::tooltip(tr("Fetch"), fetch_keys))
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(git::Fetch), cx)
                            }),
                    ),
            )
            .child(ui::divider(ui))
            .child(div().py(px(LIST_INSET)).child(list))
            .child(ui::divider(ui))
            .child(
                div()
                    .flex_none()
                    .h(px(FOOTER_HEIGHT))
                    .px_4()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.dim)
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .children(activity.map(|activity| activity.to_string())),
                    )
                    .child(ui::hint_bar(&self.hints(), ui)),
            );
        div()
            .key_context("BranchesPopup")
            .flex()
            .items_start()
            .gap(px(SUBMENU_GAP))
            .font_family(theme::UI_FONT)
            .text_size(px(theme::TEXT_MD))
            .text_color(ui.foreground)
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.step(true, false, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.step(false, false, cx)))
            .on_action(cx.listener(|this, _: &SelectNextPage, _, cx| this.step(true, true, cx)))
            .on_action(
                cx.listener(|this, _: &SelectPreviousPage, _, cx| this.step(false, true, cx)),
            )
            .on_action(cx.listener(|this, _: &Expand, window, cx| this.expand(window, cx)))
            .on_action(cx.listener(|this, _: &Collapse, _, cx| this.collapse(cx)))
            .on_action(cx.listener(|this, _: &Confirm, window, cx| this.confirm(window, cx)))
            .on_action(cx.listener(|this, _: &ToggleFavorite, _, cx| this.toggle_favorite(cx)))
            .on_action(cx.listener(|this, _: &Dismiss, _, cx| {
                if this.submenu.take().is_some() {
                    return cx.notify();
                }
                cx.emit(DismissEvent)
            }))
            .child(popover)
            .children(self.render_submenu(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(kind: RefKind, name: &str) -> Ref {
        let remote = (kind == RefKind::Remote).then(|| name.split('/').next().unwrap().to_string());
        Ref {
            kind,
            name: name.into(),
            remote,
            oid: String::new(),
            upstream: None,
            ahead: 0,
            behind: 0,
            upstream_gone: false,
            current: false,
            time: 0,
            subject: String::new(),
        }
    }

    fn input(repo: usize, local: &[&str], remote: &[&str], tags: &[&str]) -> RepoInput {
        let mut refs = Refs {
            local: local
                .iter()
                .map(|name| reference(RefKind::Local, name))
                .collect(),
            remote: remote
                .iter()
                .map(|name| reference(RefKind::Remote, name))
                .collect(),
            tags: tags
                .iter()
                .map(|name| reference(RefKind::Tag, name))
                .collect(),
            recent: Vec::new(),
            remotes: vec!["origin".into()],
        };
        if let Some(first) = refs.local.first_mut() {
            first.current = true;
        }
        RepoInput {
            repo,
            name: format!("repo{repo}"),
            branch: local.first().map(|name| name.to_string()),
            refs: Arc::new(refs),
            favorites: HashSet::new(),
        }
    }

    /// Rows as "  label" lines, indented by depth, with ▸/▾ for nodes.
    fn outline(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|row| {
                let mark = match &row.kind {
                    RowKind::Branch(_) | RowKind::Action(_) | RowKind::CreateNamed(_) => "",
                    RowKind::Note(_) => "~",
                    _ if row.expanded => "▾",
                    _ => "▸",
                };
                format!("{}{mark}{}", "  ".repeat(row.depth), row.label)
            })
            .collect()
    }

    #[test]
    fn branches_form_a_tree_by_prefix_with_favorites_on_top() {
        let mut repo = input(
            0,
            &["main", "feature/login", "feature/ui/button", "bugfix/typo"],
            &["origin/main", "origin/feature/login"],
            &["v0.1"],
        );
        repo.favorites.insert("main".into());
        repo.favorites.insert("origin/main".into());
        let rows = build_rows(&[repo], "", &HashSet::new());
        let lines = outline(&rows);
        assert_eq!(
            &lines[5..],
            [
                "▾Local",
                "  main",
                "  ▾bugfix",
                "    typo",
                "  ▾feature",
                "    ▾ui",
                "      button",
                "    login",
                "▾Remote",
                "  origin/main",
                "  ▾origin",
                "    ▾feature",
                "      login",
                "▸Tags",
            ]
        );
        // The actions come first.
        assert!(matches!(
            rows[0].kind,
            RowKind::Action(TopAction::UpdateProject)
        ));
        assert_eq!(
            rows.iter()
                .filter(|row| matches!(row.kind, RowKind::Action(_)))
                .count(),
            5
        );
    }

    #[test]
    fn nodes_open_and_close_against_their_default() {
        let repo = input(0, &["main", "feature/a"], &[], &["v1", "v2"]);
        let mut toggled = HashSet::new();
        toggled.insert(NodeKey::Folder(0, Section::Local, "feature".into()));
        toggled.insert(NodeKey::Section(0, Section::Tags));
        let lines = outline(&build_rows(&[repo], "", &toggled));
        assert_eq!(
            &lines[5..],
            ["▾Local", "  ▸feature", "  main", "▾Tags", "  v1", "  v2"]
        );
    }

    #[test]
    fn recent_branches_skip_the_current_one() {
        let mut repo = input(0, &["main", "feature/a", "fix"], &[], &[]);
        Arc::get_mut(&mut repo.refs).unwrap().recent = vec![
            "fix".into(),
            "main".into(),
            "gone".into(),
            "feature/a".into(),
        ];
        let lines = outline(&build_rows(&[repo], "", &HashSet::new()));
        assert_eq!(&lines[5..8], ["▾Recent", "  fix", "  feature/a"]);
    }

    #[test]
    fn a_query_lists_matches_flat_and_offers_a_new_branch() {
        let repo = input(
            0,
            &["main", "feature/login"],
            &["origin/feature/login"],
            &[],
        );
        let rows = build_rows(&[repo], "login", &HashSet::new());
        let lines = outline(&rows);
        assert_eq!(
            lines,
            [
                "New Branch 'login'…",
                "▾Local",
                "  feature/login",
                "▾Remote",
                "  origin/feature/login",
            ]
        );
        assert!(!rows[2].positions.is_empty());
        // An existing name isn't offered; actions match by name.
        let repo = input(0, &["main"], &[], &[]);
        let rows = build_rows(&[repo], "push", &HashSet::new());
        assert!(matches!(rows[0].kind, RowKind::Action(TopAction::Push)));
        let repo = input(0, &["main"], &[], &[]);
        let lines = outline(&build_rows(&[repo], "main", &HashSet::new()));
        assert_eq!(lines, ["▾Local", "  main"]);
    }

    #[test]
    fn several_repositories_get_nodes_and_hide_when_nothing_matches() {
        let repos = [
            input(0, &["main"], &[], &[]),
            input(1, &["dev", "feature/x"], &[], &[]),
        ];
        let lines = outline(&build_rows(&repos, "", &HashSet::new()));
        assert_eq!(
            &lines[5..],
            [
                "▾repo0",
                "  ▾Local",
                "    main",
                "▾repo1",
                "  ▾Local",
                "    ▾feature",
                "      x",
                "    dev",
            ]
        );
        let lines = outline(&build_rows(&repos, "feat", &HashSet::new()));
        assert_eq!(
            lines,
            ["New Branch 'feat'…", "▾repo1", "  ▾Local", "    feature/x"]
        );
    }

    fn labels(items: &[Option<SubItem>]) -> Vec<String> {
        items
            .iter()
            .map(|item| {
                item.as_ref()
                    .map_or("—".to_string(), |item| item.label.clone())
            })
            .collect()
    }

    fn branch(kind: RefKind, name: &str, current: bool, upstream: bool) -> BranchRow {
        BranchRow {
            repo: 0,
            section: Section::Local,
            kind,
            name: name.into(),
            current,
            favorite: false,
            ahead: 0,
            behind: 0,
            upstream: upstream.then(|| format!("origin/{name}")),
        }
    }

    #[test]
    fn submenus_follow_jetbrains() {
        let local = submenu_items(&branch(RefKind::Local, "feature/x", false, true), "main");
        assert_eq!(
            labels(&local),
            [
                "Checkout",
                "New Branch from 'feature/x'…",
                "Checkout and Rebase onto 'main'",
                "—",
                "Compare with 'main'",
                "Show Diff with Working Tree",
                "—",
                "Rebase 'main' onto 'feature/x'",
                "Merge 'feature/x' into 'main'",
                "—",
                "Update",
                "Push…",
                "—",
                "Rename…",
                "Delete",
            ]
        );
        let current = submenu_items(&branch(RefKind::Local, "main", true, false), "main");
        assert_eq!(
            labels(&current),
            ["New Branch from 'main'…", "—", "Push…", "—", "Rename…"]
        );
        let remote = submenu_items(&branch(RefKind::Remote, "origin/x", false, false), "main");
        assert_eq!(
            labels(&remote),
            [
                "Checkout",
                "New Branch from 'origin/x'…",
                "Checkout and Rebase onto 'main'",
                "—",
                "Compare with 'main'",
                "Show Diff with Working Tree",
                "—",
                "Rebase 'main' onto 'origin/x'",
                "Merge 'origin/x' into 'main'",
                "Pull into 'main' Using Rebase",
                "Pull into 'main' Using Merge",
                "—",
                "Delete",
            ]
        );
        let tag = submenu_items(&branch(RefKind::Tag, "v1", false, false), "a1b2c3d");
        assert_eq!(
            labels(&tag),
            [
                "Checkout",
                "New Branch from 'v1'…",
                "—",
                "Compare with 'a1b2c3d'",
                "Show Diff with Working Tree",
                "—",
                "Merge 'v1' into 'a1b2c3d'",
                "—",
                "Delete",
            ]
        );
        // Each item dispatches the action of its branch.
        let action = local[0].as_ref().unwrap().action.boxed_clone();
        assert!(action.partial_eq(&git::CheckoutRef {
            repo: 0,
            name: "feature/x".into(),
            kind: RefKind::Local,
        }));
    }
}
