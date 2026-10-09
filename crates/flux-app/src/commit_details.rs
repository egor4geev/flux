//! The commit pane of the Git window (part C of stage 6.3), as the one of JetBrains' log: on top the
//! selected commit's changed files as a tree by directory (or a flat list), below the message, the
//! hash (a click copies it), the author and the date, the committer when another, the parents (a
//! click shows one in the log) and the branches that contain the commit ("In 3 branches: …", read
//! after the rest). Several selected commits: the files of all of them, the messages one by one.
//!
//! A double click, ↵ or ⌘D on a file opens its diff with the parent (`git::OpenCompareDiff`); with
//! several commits — from before the oldest one that touched it to the newest; F4 opens the file.
//! Everything is read in the background; a newer selection drops the answers to an older one.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use flux_git::{CommitDetails, FileChange, FileStatus, GitError};
use gpui::{
    App, ClickEvent, ClipboardItem, Context, Entity, FocusHandle, Focusable, FontWeight,
    InteractiveElement, IntoElement, KeyBinding, ParentElement, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement, Styled, Window, actions, div, prelude::*, px,
};

use crate::diff_view::DiffSide;
use crate::git::{self, GitStore};
use crate::i18n::{tr, trf, trn};
use crate::icons::{IconName, file_icon, folder_icon, icon};
use crate::log_actions::OpenCommitFile;
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, RADIUS_SM};

actions!(
    commit_details,
    [
        SelectNext,
        SelectPrevious,
        Expand,
        Collapse,
        ShowDiff,
        JumpToSource,
        ToggleTree,
        ExpandAll,
        CollapseAll
    ]
);

/// git's empty tree: the left side of a root commit's files.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
/// At most this many selected commits are read in detail.
const MAX_COMMITS: usize = 50;
const ROW_HEIGHT: f32 = 24.;
const INDENT: f32 = 14.;

pub fn init(cx: &mut App) {
    let context = Some("CommitDetails");
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("up", SelectPrevious, context),
        KeyBinding::new("right", Expand, context),
        KeyBinding::new("left", Collapse, context),
        KeyBinding::new("enter", ShowDiff, context),
        KeyBinding::new("cmd-d", ShowDiff, context),
        KeyBinding::new("f4", JumpToSource, context),
    ]);
}

/// A changed file of the shown commits and the revisions its diff compares.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ChangedFile {
    change: FileChange,
    /// The left side: the parent of the oldest shown commit that touched it.
    from: String,
    /// The right side: the newest shown commit that touched it.
    to: String,
}

/// What was read for a selection.
#[derive(Debug, Clone)]
enum Loaded {
    Reading,
    Failed(String),
    Ready {
        /// Newest first, as selected.
        commits: Vec<CommitDetails>,
        files: Vec<ChangedFile>,
    },
}

/// A row of the files tree.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Row {
    /// A directory (a chain of single directories is one row: `src/app/ui`).
    Dir {
        path: String,
        label: String,
        depth: usize,
        expanded: bool,
    },
    File {
        index: usize,
        depth: usize,
    },
}

pub struct CommitDetailsView {
    git: Entity<GitStore>,
    focus_handle: FocusHandle,
    scroll: ScrollHandle,
    /// The repository, the selected commits (newest first) and, in a file's history, the file.
    shown: Option<(usize, Vec<String>, Option<String>)>,
    loaded: Loaded,
    /// The branches containing the (single) shown commit, once read.
    branches: Option<Result<Vec<String>, String>>,
    /// Answers to an older selection are dropped.
    generation: u64,
    tree: bool,
    collapsed: HashSet<String>,
    rows: Vec<Row>,
    selected: Option<usize>,
}

impl CommitDetailsView {
    pub fn new(git: Entity<GitStore>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            git,
            focus_handle: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            shown: None,
            loaded: Loaded::Reading,
            branches: None,
            generation: 0,
            tree: true,
            collapsed: HashSet::new(),
            rows: Vec::new(),
            selected: None,
        }
    }

    /// Shows the commits selected in the log (newest first): one — in full; several — their files
    /// together; none — a hint. `path` — a file's history: that file (relative) is selected.
    pub fn show(
        &mut self,
        repo: usize,
        oids: Vec<String>,
        path: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let shown = Some((repo, oids.clone(), path.clone()));
        if self.shown == shown {
            return;
        }
        self.shown = shown;
        self.generation += 1;
        self.branches = None;
        self.rows.clear();
        self.selected = None;
        cx.notify();
        if oids.is_empty() {
            return;
        }
        self.loaded = Loaded::Reading;
        let generation = self.generation;
        let read = self
            .git
            .update(cx, |git, cx| git.commits_details(repo, oids, cx));
        cx.spawn(async move |this, cx| {
            let result = read.await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.loaded = match result {
                    Ok((commits, files)) => Loaded::Ready { commits, files },
                    Err(error) => Loaded::Failed(error.to_string()),
                };
                this.rebuild();
                this.selected = this.initial_selection(path.as_deref());
                this.read_branches(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// "In N branches" of a single commit: read after the rest (it may take a while).
    fn read_branches(&mut self, cx: &mut Context<Self>) {
        let (Some((repo, oids, _)), Loaded::Ready { .. }) = (&self.shown, &self.loaded) else {
            return;
        };
        let [oid] = oids.as_slice() else {
            return;
        };
        let (repo, oid) = (*repo, oid.clone());
        let generation = self.generation;
        let read = self.git.update(cx, |git, cx| {
            git.read(repo, cx, move |repo| {
                flux_git::log::branches_containing(repo, &oid)
            })
        });
        cx.spawn(async move |this, cx| {
            let result = read.await;
            this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.branches = Some(result.map_err(|error| error.to_string()));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn files(&self) -> &[ChangedFile] {
        match &self.loaded {
            Loaded::Ready { files, .. } => files,
            _ => &[],
        }
    }

    fn rebuild(&mut self) {
        let paths: Vec<&str> = self
            .files()
            .iter()
            .map(|f| f.change.path.as_str())
            .collect();
        self.rows = if self.tree {
            tree_rows(&paths, &self.collapsed)
        } else {
            (0..paths.len())
                .map(|index| Row::File { index, depth: 0 })
                .collect()
        };
    }

    /// The row of the history's file, else the first file.
    fn initial_selection(&self, path: Option<&str>) -> Option<usize> {
        let files = self.files();
        let wanted = path.and_then(|path| files.iter().position(|f| f.change.path == path));
        let first = self
            .rows
            .iter()
            .position(|row| matches!(row, Row::File { .. }));
        wanted
            .and_then(|index| {
                self.rows.iter().position(|row| {
                    *row == Row::File {
                        index,
                        depth: row_depth(row),
                    }
                })
            })
            .or(first)
    }

    fn move_selection(&mut self, step: isize, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let at = match self.selected {
            Some(at) => (at as isize + step).clamp(0, self.rows.len() as isize - 1) as usize,
            None => 0,
        };
        self.selected = Some(at);
        self.scroll.scroll_to_item(at);
        cx.notify();
    }

    fn set_expanded(&mut self, expanded: bool, cx: &mut Context<Self>) {
        let Some(Row::Dir { path, .. }) = self.selected.and_then(|at| self.rows.get(at)).cloned()
        else {
            // On a file, ← goes to its directory.
            if !expanded && let Some(at) = self.selected {
                let depth = row_depth(&self.rows[at]);
                if let Some(parent) = (0..at).rev().find(|&i| row_depth(&self.rows[i]) < depth) {
                    self.selected = Some(parent);
                    cx.notify();
                }
            }
            return;
        };
        if expanded {
            self.collapsed.remove(&path);
        } else {
            self.collapsed.insert(path);
        }
        self.rebuild();
        cx.notify();
    }

    fn toggle_dir(&mut self, path: &str, cx: &mut Context<Self>) {
        if !self.collapsed.remove(path) {
            self.collapsed.insert(path.to_string());
        }
        self.rebuild();
        cx.notify();
    }

    fn set_all(&mut self, expanded: bool, cx: &mut Context<Self>) {
        self.collapsed.clear();
        if !expanded {
            let paths: Vec<&str> = self
                .files()
                .iter()
                .map(|f| f.change.path.as_str())
                .collect();
            self.collapsed = tree_rows(&paths, &HashSet::new())
                .into_iter()
                .filter_map(|row| match row {
                    Row::Dir { path, .. } => Some(path),
                    Row::File { .. } => None,
                })
                .collect();
        }
        self.rebuild();
        self.selected = self
            .selected
            .map(|at| at.min(self.rows.len().saturating_sub(1)));
        cx.notify();
    }

    fn selected_file(&self) -> Option<ChangedFile> {
        match self.selected.and_then(|at| self.rows.get(at))? {
            Row::File { index, .. } => self.files().get(*index).cloned(),
            Row::Dir { .. } => None,
        }
    }

    fn open_diff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(file) = self.selected_file() else {
            return self.set_expanded(true, cx);
        };
        let Some((repo, _, _)) = &self.shown else {
            return;
        };
        let repo = *repo;
        let Some(path) = self.absolute(repo, &file.change.path, cx) else {
            return;
        };
        let (left, right) = diff_sides(&file);
        window.dispatch_action(
            Box::new(git::OpenCompareDiff {
                repo,
                path,
                left,
                right,
            }),
            cx,
        );
    }

    fn jump_to_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(file), Some((repo, _, _))) = (self.selected_file(), &self.shown) else {
            return;
        };
        if let Some(path) = self.absolute(*repo, &file.change.path, cx) {
            window.dispatch_action(Box::new(OpenCommitFile { path }), cx);
        }
    }

    fn absolute(&self, repo: usize, relative: &str, cx: &App) -> Option<PathBuf> {
        let git = self.git.read(cx);
        Some(git.repos().get(repo)?.repo.absolute(relative))
    }

    fn select_row(
        &mut self,
        at: usize,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        self.selected = Some(at);
        cx.notify();
        match self.rows.get(at).cloned() {
            Some(Row::Dir { path, .. }) if event.click_count() == 2 => self.toggle_dir(&path, cx),
            Some(Row::File { .. }) if event.click_count() == 2 => self.open_diff(window, cx),
            _ => {}
        }
    }
}

/// What a selection reads: the commits in detail and their files together.
type Details = (Vec<CommitDetails>, Vec<ChangedFile>);

impl GitStore {
    /// The details of commits (newest first, at most [`MAX_COMMITS`]) and the files they changed
    /// together.
    fn commits_details(
        &self,
        repo: usize,
        oids: Vec<String>,
        cx: &mut Context<Self>,
    ) -> gpui::Task<Result<Details, GitError>> {
        self.read(repo, cx, move |repo| {
            let commits = oids
                .iter()
                .take(MAX_COMMITS)
                .map(|oid| flux_git::log::commit_details(repo, oid))
                .collect::<Result<Vec<_>, _>>()?;
            let files = merge_files(&commits);
            Ok((commits, files))
        })
    }
}

/// The files of several commits (newest first) together: a file once, with the newest change's
/// status and paths; its diff goes from before the oldest commit that touched it to the newest.
fn merge_files(commits: &[CommitDetails]) -> Vec<ChangedFile> {
    let mut files: Vec<ChangedFile> = Vec::new();
    for details in commits {
        let parent = details
            .commit
            .parents
            .first()
            .cloned()
            .unwrap_or_else(|| EMPTY_TREE.to_string());
        for change in &details.files {
            // An older commit's file under its newer path (before a rename) or the same path.
            let known = files.iter_mut().find(|file| {
                file.change.path == change.path
                    || file.change.orig_path.as_deref() == Some(change.path.as_str())
            });
            match known {
                Some(file) => {
                    file.from = parent.clone();
                    if file.change.orig_path.as_deref() == Some(change.path.as_str()) {
                        file.change.orig_path =
                            change.orig_path.clone().or(Some(change.path.clone()));
                    }
                    if change.status == FileStatus::Added {
                        file.change.status = FileStatus::Added;
                        file.change.orig_path = None;
                    }
                }
                None => files.push(ChangedFile {
                    change: change.clone(),
                    from: parent.clone(),
                    to: details.commit.oid.clone(),
                }),
            }
        }
    }
    files.sort_by(|a, b| a.change.path.cmp(&b.change.path));
    files
}

/// The sides of a file's diff: the file before (its old path after a rename) and after.
fn diff_sides(file: &ChangedFile) -> (DiffSide, DiffSide) {
    let short = |rev: &str| -> String {
        if rev == EMPTY_TREE {
            tr("Empty").to_string()
        } else {
            rev.chars().take(8).collect()
        }
    };
    let left_path = file
        .change
        .orig_path
        .clone()
        .unwrap_or_else(|| file.change.path.clone());
    (
        DiffSide::Revision {
            rev: file.from.clone(),
            path: left_path,
            label: short(&file.from),
        },
        DiffSide::Revision {
            rev: file.to.clone(),
            path: file.change.path.clone(),
            label: short(&file.to),
        },
    )
}

fn row_depth(row: &Row) -> usize {
    match row {
        Row::Dir { depth, .. } | Row::File { depth, .. } => *depth,
    }
}

/// A directory of the tree being built: subdirectories and files (indexes into the paths).
#[derive(Default)]
struct Node {
    dirs: BTreeMap<String, Node>,
    files: Vec<usize>,
}

/// The rows of a tree of paths (sorted): directories first, chains of single directories joined.
fn tree_rows(paths: &[&str], collapsed: &HashSet<String>) -> Vec<Row> {
    let mut root = Node::default();
    for (index, path) in paths.iter().enumerate() {
        let mut node = &mut root;
        let mut parts: Vec<&str> = path.split('/').collect();
        parts.pop();
        for part in parts {
            node = node.dirs.entry(part.to_string()).or_default();
        }
        node.files.push(index);
    }
    let mut rows = Vec::new();
    push_node(&root, "", 0, paths, collapsed, &mut rows);
    rows
}

fn push_node(
    node: &Node,
    prefix: &str,
    depth: usize,
    paths: &[&str],
    collapsed: &HashSet<String>,
    rows: &mut Vec<Row>,
) {
    for (name, mut child) in &node.dirs {
        let mut label = name.clone();
        let mut path = format!("{prefix}{name}");
        while child.files.is_empty() && child.dirs.len() == 1 {
            let (next, grandchild) = child.dirs.iter().next().expect("one directory");
            label = format!("{label}/{next}");
            path = format!("{path}/{next}");
            child = grandchild;
        }
        let expanded = !collapsed.contains(&path);
        rows.push(Row::Dir {
            path: path.clone(),
            label,
            depth,
            expanded,
        });
        if expanded {
            push_node(
                child,
                &format!("{path}/"),
                depth + 1,
                paths,
                collapsed,
                rows,
            );
        }
    }
    let mut files = node.files.clone();
    files.sort_by(|a, b| flux_fs::compare_names(file_name(paths[*a]), file_name(paths[*b])));
    rows.extend(files.into_iter().map(|index| Row::File { index, depth }));
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// "2026-10-09 14:32" in the local time zone.
pub(crate) fn format_time(seconds: i64) -> String {
    let local = seconds + utc_offset(seconds);
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        secs / 3600,
        secs % 3600 / 60
    )
}

/// Days since 1970-01-01 → (year, month, day) (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(target_os = "macos")]
fn utc_offset(seconds: i64) -> i64 {
    use core_foundation_sys::date::CFAbsoluteTime;
    use core_foundation_sys::timezone::{CFTimeZoneCopySystem, CFTimeZoneGetSecondsFromGMT};
    // CFAbsoluteTime counts from 2001-01-01.
    let at = (seconds - 978_307_200) as CFAbsoluteTime;
    // SAFETY: the system time zone is a valid CF object, released after use.
    unsafe {
        let zone = CFTimeZoneCopySystem();
        if zone.is_null() {
            return 0;
        }
        let offset = CFTimeZoneGetSecondsFromGMT(zone, at);
        core_foundation_sys::base::CFRelease(zone as _);
        offset as i64
    }
}

#[cfg(not(target_os = "macos"))]
fn utc_offset(_: i64) -> i64 {
    0
}

impl Focusable for CommitDetailsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl CommitDetailsView {
    fn render_toolbar(&self, count: usize, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        div()
            .flex_none()
            .h(px(32.))
            .px_2()
            .flex()
            .items_center()
            .gap_1()
            .child(ui::section_label(tr("Changed files"), ui))
            .child(
                div()
                    .text_size(px(theme::TEXT_XS))
                    .text_color(ui.dim)
                    .child(count.to_string()),
            )
            .child(div().flex_1())
            .child(
                ui::toggle_button("details-tree", IconName::Folder, self.tree, ui)
                    .tooltip(ui::tooltip(tr("Group by Directory"), None))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_tree(cx))),
            )
            .child(
                ui::icon_button("details-expand", IconName::ExpandAll, ui)
                    .tooltip(ui::tooltip(tr("Expand All"), None))
                    .on_click(cx.listener(|this, _, _, cx| this.set_all(true, cx))),
            )
            .child(
                ui::icon_button("details-collapse", IconName::CollapseAll, ui)
                    .tooltip(ui::tooltip(tr("Collapse All"), None))
                    .on_click(cx.listener(|this, _, _, cx| this.set_all(false, cx))),
            )
    }

    fn toggle_tree(&mut self, cx: &mut Context<Self>) {
        self.tree = !self.tree;
        self.rebuild();
        self.selected = self
            .rows
            .iter()
            .position(|row| matches!(row, Row::File { .. }));
        cx.notify();
    }

    fn render_rows(&self, focused: bool, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let ui = Theme::ui(cx);
        let files = self.files();
        let highlighted = self.shown.as_ref().and_then(|(_, _, path)| path.clone());
        self.rows
            .iter()
            .enumerate()
            .map(|(at, row)| {
                let selected = self.selected == Some(at);
                let base = div()
                    .id(("details-row", at))
                    .h(px(ROW_HEIGHT))
                    .pl(px(8. + row_depth(row) as f32 * INDENT))
                    .pr_2()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .rounded(px(RADIUS_SM))
                    .whitespace_nowrap()
                    .cursor_pointer()
                    .map(|row| match (selected, focused) {
                        (true, true) => row.bg(ui.list_selected),
                        (true, false) => row.bg(ui.list_selected_inactive),
                        _ => row.hover(move |style| style.bg(ui.hover)),
                    })
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        this.select_row(at, event, window, cx)
                    }));
                match row {
                    Row::Dir {
                        label, expanded, ..
                    } => base
                        .child(
                            icon(
                                if *expanded {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                },
                                ui.dim,
                            )
                            .size(px(12.)),
                        )
                        .child(folder_icon(*expanded, &ui).render().size(px(14.)))
                        .child(div().text_color(ui.text_muted).child(label.clone()))
                        .into_any_element(),
                    Row::File { index, .. } => {
                        let file = &files[*index];
                        file_row(base, &file.change, self.tree, highlighted.as_deref(), ui)
                            .into_any_element()
                    }
                }
            })
            .collect()
    }

    fn render_info(&self, commits: &[CommitDetails], cx: &mut Context<Self>) -> gpui::AnyElement {
        let ui = Theme::ui(cx);
        let Some((repo, _, _)) = &self.shown else {
            return div().into_any_element();
        };
        let repo = *repo;
        if let [details] = commits {
            let commit = &details.commit;
            let oid = commit.oid.clone();
            let author = format!("{} <{}>", commit.author, commit.author_email);
            let committer = (commit.committer != commit.author
                || details.committer_email != commit.author_email)
                .then(|| {
                    trf(
                        "committed by {0} on {1}",
                        &[&commit.committer, &format_time(commit.commit_time)],
                    )
                });
            let branches: SharedString = match &self.branches {
                None => tr("In branches: reading…").into(),
                Some(Err(error)) => error.clone().into(),
                Some(Ok(branches)) if branches.is_empty() => tr("In no branch").into(),
                Some(Ok(branches)) => format!(
                    "{}: {}",
                    trn(branches.len(), "In {n} branch", "In {n} branches"),
                    branches.join(", ")
                )
                .into(),
            };
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .font_family(theme::code_font())
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.foreground)
                        .child(details.message.clone()),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_1()
                        .text_size(px(theme::TEXT_SM))
                        .child(
                            div()
                                .id("details-hash")
                                .px_1()
                                .rounded(px(RADIUS_SM))
                                .font_family(theme::code_font())
                                .text_color(ui.accent_text)
                                .cursor_pointer()
                                .hover(move |style| style.bg(ui.hover))
                                .tooltip(ui::tooltip(tr("Copy Revision Number"), None))
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(oid.clone()))
                                })
                                .child(commit.short().to_string()),
                        )
                        .child(div().text_color(ui.text_muted).child(author))
                        .child(
                            div()
                                .text_color(ui.dim)
                                .child(trf("on {0}", &[&format_time(commit.author_time)])),
                        ),
                )
                .children(committer.map(|text| {
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.dim)
                        .child(text)
                }))
                .when(!commit.parents.is_empty(), |info| {
                    info.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_size(px(theme::TEXT_SM))
                            .child(div().text_color(ui.dim).child(trn(
                                commit.parents.len(),
                                "Parent",
                                "Parents",
                            )))
                            .children(commit.parents.iter().enumerate().map(|(at, parent)| {
                                let action = git::ShowCommitInLog {
                                    repo,
                                    oid: parent.clone(),
                                };
                                div()
                                    .id(("details-parent", at))
                                    .px_1()
                                    .rounded(px(RADIUS_SM))
                                    .font_family(theme::code_font())
                                    .text_color(ui.accent_text)
                                    .cursor_pointer()
                                    .hover(move |style| style.bg(ui.hover))
                                    .on_click(move |_, window, cx| {
                                        window.dispatch_action(Box::new(action.clone()), cx)
                                    })
                                    .child(parent.chars().take(8).collect::<String>())
                            })),
                    )
                })
                .child(
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.dim)
                        .child(branches),
                )
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(ui::section_label(
                    trn(commits.len(), "{n} commit", "{n} commits"),
                    ui,
                ))
                .children(commits.iter().map(|details| {
                    let commit = &details.commit;
                    div()
                        .flex()
                        .gap_2()
                        .text_size(px(theme::TEXT_SM))
                        .child(
                            div()
                                .flex_none()
                                .font_family(theme::code_font())
                                .text_color(ui.accent_text)
                                .child(commit.short().to_string()),
                        )
                        .child(
                            div()
                                .text_color(ui.foreground)
                                .child(commit.summary.clone()),
                        )
                        .child(div().text_color(ui.dim).child(commit.author.clone()))
                }))
                .into_any_element()
        }
    }
}

/// A file's row content: its icon, the name in its status color (struck through when deleted) and,
/// dimmed, "← old path" after a rename (in a flat list, its directory).
fn file_row(
    row: gpui::Stateful<gpui::Div>,
    file: &FileChange,
    tree: bool,
    highlighted: Option<&str>,
    ui: UiColors,
) -> gpui::Stateful<gpui::Div> {
    let (dir, name) = match file.path.rsplit_once('/') {
        Some((dir, name)) => (Some(dir.to_string()), name.to_string()),
        None => (None, file.path.clone()),
    };
    let detail = match (&file.orig_path, tree) {
        (Some(orig), _) => Some(trf("← {0}", &[orig])),
        (None, false) => dir,
        (None, true) => None,
    };
    let mine = highlighted == Some(file.path.as_str());
    row.child(file_icon(&name, &ui).render().size(px(14.)))
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_color(git::status_color(file.status, &ui))
                .when(mine, |name| name.font_weight(FontWeight::SEMIBOLD))
                .when(file.status == FileStatus::Deleted, |name| {
                    name.line_through()
                })
                .child(name),
        )
        .children(detail.map(|detail| {
            div()
                .min_w_0()
                .truncate()
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.dim)
                .child(detail)
        }))
}

impl Render for CommitDetailsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let focused = self.focus_handle.contains_focused(window, cx);
        let root = div()
            .key_context("CommitDetails")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &Expand, _, cx| this.set_expanded(true, cx)))
            .on_action(cx.listener(|this, _: &Collapse, _, cx| this.set_expanded(false, cx)))
            .on_action(cx.listener(|this, _: &ShowDiff, window, cx| this.open_diff(window, cx)))
            .on_action(
                cx.listener(|this, _: &JumpToSource, window, cx| this.jump_to_source(window, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleTree, _, cx| this.toggle_tree(cx)))
            .on_action(cx.listener(|this, _: &ExpandAll, _, cx| this.set_all(true, cx)))
            .on_action(cx.listener(|this, _: &CollapseAll, _, cx| this.set_all(false, cx)))
            .size_full()
            .flex()
            .flex_col()
            .text_size(px(theme::TEXT_MD));
        let note = |text: SharedString, color| {
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .child(icon(IconName::Commit, ui.dim).size(px(20.)))
                .child(div().text_color(color).child(text))
        };
        let empty = self
            .shown
            .as_ref()
            .is_none_or(|(_, oids, _)| oids.is_empty());
        if empty {
            return root.child(note(tr("Select a commit").into(), ui.text_muted));
        }
        let commits = match &self.loaded {
            Loaded::Reading => return root.child(note(tr("Reading…").into(), ui.text_muted)),
            Loaded::Failed(error) => return root.child(note(error.clone().into(), ui.error)),
            Loaded::Ready { commits, .. } => commits.clone(),
        };
        let count = self.files().len();
        let rows = self.render_rows(focused, cx);
        let info = self.render_info(&commits, cx);
        root.child(self.render_toolbar(count, cx))
            .child(
                div()
                    .id("details-files")
                    .flex_1()
                    .min_h(px(80.))
                    .px_1()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .when(count == 0, |list| {
                        list.child(div().p_2().text_color(ui.dim).child(tr("No changed files")))
                    })
                    .children(rows),
            )
            .child(div().mx_2().child(ui::divider(ui)))
            .child(
                div()
                    .id("details-info")
                    .flex_none()
                    .max_h(px(260.))
                    .p_3()
                    .overflow_y_scroll()
                    .child(info),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_git::LogCommit;

    fn commit(
        oid: &str,
        parent: Option<&str>,
        files: &[(FileStatus, &str, Option<&str>)],
    ) -> CommitDetails {
        CommitDetails {
            commit: LogCommit {
                oid: oid.into(),
                parents: parent.into_iter().map(String::from).collect(),
                summary: String::new(),
                author: String::new(),
                author_email: String::new(),
                author_time: 0,
                committer: String::new(),
                commit_time: 0,
            },
            message: String::new(),
            committer_email: String::new(),
            files: files
                .iter()
                .map(|(status, path, orig)| FileChange {
                    status: *status,
                    path: path.to_string(),
                    orig_path: orig.map(String::from),
                })
                .collect(),
        }
    }

    #[test]
    fn dates_are_civil() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_735), (2026, 10, 9));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn trees_join_single_directories() {
        let paths = ["a/b/c/x.rs", "a/b/c/y.rs", "a/z.rs", "top.rs"];
        let rows = tree_rows(&paths, &HashSet::new());
        let labels: Vec<String> = rows
            .iter()
            .map(|row| match row {
                Row::Dir { label, depth, .. } => format!("{depth}:{label}/"),
                Row::File { index, depth } => format!("{depth}:{}", paths[*index]),
            })
            .collect();
        assert_eq!(
            labels,
            [
                "0:a/",
                "1:b/c/",
                "2:a/b/c/x.rs",
                "2:a/b/c/y.rs",
                "1:a/z.rs",
                "0:top.rs"
            ]
        );
        let collapsed = HashSet::from(["a".to_string()]);
        assert_eq!(tree_rows(&paths, &collapsed).len(), 2);
    }

    #[test]
    fn files_of_several_commits_span_them() {
        // Newest first: c3 renames old.rs → new.rs, c1 modified old.rs; c2 adds b.rs.
        let commits = [
            commit(
                "c3",
                Some("c2"),
                &[(FileStatus::Renamed, "new.rs", Some("old.rs"))],
            ),
            commit("c2", Some("c1"), &[(FileStatus::Added, "b.rs", None)]),
            commit("c1", None, &[(FileStatus::Modified, "old.rs", None)]),
        ];
        let files = merge_files(&commits);
        assert_eq!(files.len(), 2);
        let renamed = files.iter().find(|f| f.change.path == "new.rs").unwrap();
        assert_eq!(
            (renamed.from.as_str(), renamed.to.as_str()),
            (EMPTY_TREE, "c3")
        );
        assert_eq!(renamed.change.orig_path.as_deref(), Some("old.rs"));
        let added = files.iter().find(|f| f.change.path == "b.rs").unwrap();
        assert_eq!((added.from.as_str(), added.to.as_str()), ("c1", "c2"));
    }
}
