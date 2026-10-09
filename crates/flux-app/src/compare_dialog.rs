//! Comparing branches, as JetBrains IDEs do: Compare with Current — the commits that are in the
//! chosen branch and not in the current one and the other way round (with the files of the selected
//! commit), and the files that differ between their tips; Show Diff with Working Tree — the files
//! that differ between a branch and the working tree. A double click (or ↵) on a file opens a
//! comparison diff tab (`git::OpenCompareDiff`); the dialog closes for it.
//!
//! Keys: ↑↓ along a list, → / ← between the commits and the selected commit's files, ⇥ between
//! Commits and Files, ↵ the diff, Esc closes.

use std::collections::HashMap;
use std::ops::Range;

use flux_git::{CommitInfo, FileChange, FileStatus, GitError};
use gpui::{
    AnyElement, App, ClickEvent, Context, DismissEvent, Div, Entity, EventEmitter, FocusHandle,
    Focusable, FontWeight, KeyBinding, Render, ScrollStrategy, SharedString,
    UniformListScrollHandle, Window, actions, div, prelude::*, px, uniform_list,
};

use crate::diff_view::DiffSide;
use crate::git::{self, GitStore};
use crate::i18n::{tr, trf};
use crate::icons::{IconName, file_icon, icon};
use crate::push_dialog::{age, note, now_seconds};
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, RADIUS_SM};
use crate::workspace::Workspace;

actions!(
    compare_dialog,
    [
        SelectNext,
        SelectPrevious,
        FocusFiles,
        FocusCommits,
        SwitchTab,
        OpenDiff,
        Dismiss
    ]
);

const WIDTH: f32 = 820.;
const HEIGHT: f32 = 520.;
const FILES_WIDTH: f32 = 300.;
const ROW_HEIGHT: f32 = 28.;

pub fn init(cx: &mut App) {
    let context = Some("CompareDialog");
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("up", SelectPrevious, context),
        KeyBinding::new("right", FocusFiles, context),
        KeyBinding::new("left", FocusCommits, context),
        KeyBinding::new("tab", SwitchTab, context),
        KeyBinding::new("enter", OpenDiff, context),
        KeyBinding::new("escape", Dismiss, context),
    ]);
}

/// The dialog's window-level actions: `CompareWithCurrent`, `DiffWithWorkingTree`.
pub fn workspace_actions(root: Div, cx: &mut Context<Workspace>) -> Div {
    root.on_action(
        cx.listener(|this, action: &git::CompareWithCurrent, window, cx| {
            open(this, action.repo, action.name.clone(), true, window, cx)
        }),
    )
    .on_action(
        cx.listener(|this, action: &git::DiffWithWorkingTree, window, cx| {
            open(this, action.repo, action.name.clone(), false, window, cx)
        }),
    )
}

fn open(
    workspace: &mut Workspace,
    repo: usize,
    target: String,
    with_current: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let git = workspace.git().clone();
    if git.read(cx).repos().get(repo).is_none() {
        return;
    }
    workspace.toggle_dialog(window, cx, move |_, cx| {
        CompareDialog::new(git, repo, target, with_current, cx)
    });
}

/// A file that differs, with its old path after a rename (the left side's path).
type Changed = FileChange;

/// The left side's path of a file: where a renamed file came from.
fn left_path(file: &Changed) -> String {
    match (file.status, &file.orig_path) {
        (FileStatus::Renamed, Some(orig)) => orig.clone(),
        _ => file.path.clone(),
    }
}

impl GitStore {
    /// The files a commit changed against its first parent, renames with their old paths (the
    /// first commit: everything it added).
    fn commit_changes(
        &self,
        repo: usize,
        oid: &str,
        cx: &mut gpui::Context<Self>,
    ) -> gpui::Task<Result<Vec<Changed>, GitError>> {
        let oid = oid.to_string();
        self.read(repo, cx, move |repo| {
            match flux_git::diff_changes(repo, &format!("{oid}^"), Some(&oid)) {
                Ok(files) => Ok(files),
                // No parent: the root commit.
                Err(GitError::Failed { .. }) => Ok(flux_git::commit_files(repo, &oid)?
                    .into_iter()
                    .map(|(status, path)| FileChange {
                        status,
                        path,
                        orig_path: None,
                    })
                    .collect()),
                Err(err) => Err(err),
            }
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Commits,
    Files,
}

/// Something read in the background.
enum Loading<T> {
    Reading,
    Ready(T),
    Failed(String),
}

/// A row of the commit list: a section's header, or a commit of the section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommitRow {
    /// `true` — commits of the target that the current branch lacks.
    Header(bool),
    Commit(bool, usize),
}

pub struct CompareDialog {
    git: Entity<GitStore>,
    repo: usize,
    /// The branch (or tag, revision) the user chose.
    target: String,
    /// What it is compared with: the current branch (its name, or `HEAD` when detached) and its
    /// label; `None` — the working tree.
    current: Option<(String, String)>,
    tab: Tab,
    /// In the target and not in the current branch; in the current branch and not in the target.
    commits: Loading<(Vec<CommitInfo>, Vec<CommitInfo>)>,
    rows: Vec<CommitRow>,
    selected_commit: Option<usize>,
    /// The commit list or the selected commit's files has the keyboard.
    in_commit_files: bool,
    commit_files: HashMap<String, Loading<Vec<Changed>>>,
    selected_commit_file: usize,
    files: Loading<Vec<Changed>>,
    selected_file: usize,
    commit_scroll: UniformListScrollHandle,
    file_scroll: UniformListScrollHandle,
    focus_handle: FocusHandle,
}

impl EventEmitter<DismissEvent> for CompareDialog {}

impl CompareDialog {
    fn new(
        git: Entity<GitStore>,
        repo: usize,
        target: String,
        with_current: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let current = with_current.then(|| {
            let store = git.read(cx);
            match store.current_branch(repo) {
                Some(branch) => (branch.clone(), branch),
                None => {
                    let short = store
                        .repos()
                        .get(repo)
                        .and_then(|entry| entry.status.branch.oid.clone())
                        .map(|oid| oid.chars().take(7).collect::<String>())
                        .unwrap_or_else(|| "HEAD".into());
                    ("HEAD".to_string(), short)
                }
            }
        });
        let mut dialog = Self {
            git,
            repo,
            target,
            current,
            tab: if with_current {
                Tab::Commits
            } else {
                Tab::Files
            },
            commits: Loading::Reading,
            rows: Vec::new(),
            selected_commit: None,
            in_commit_files: false,
            commit_files: HashMap::new(),
            selected_commit_file: 0,
            files: Loading::Reading,
            selected_file: 0,
            commit_scroll: UniformListScrollHandle::new(),
            file_scroll: UniformListScrollHandle::new(),
            focus_handle: cx.focus_handle(),
        };
        dialog.load(cx);
        dialog
    }

    /// Reads the commits both ways (comparing with the current branch) and the files.
    fn load(&mut self, cx: &mut Context<Self>) {
        let (repo, target) = (self.repo, self.target.clone());
        let from_to = match &self.current {
            Some((current, _)) => (current.clone(), Some(target.clone())),
            None => (target.clone(), None),
        };
        let files = self.git.update(cx, |git, cx| {
            git.diff_changes(repo, &from_to.0, from_to.1.as_deref(), cx)
        });
        cx.spawn(async move |this, cx| {
            let files = files.await;
            this.update(cx, |this, cx| {
                this.files = match files {
                    Ok(files) => Loading::Ready(files),
                    Err(err) => Loading::Failed(err.to_string()),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
        let Some((current, _)) = self.current.clone() else {
            return;
        };
        let commits = self.git.update(cx, |git, cx| {
            git.compare_commits(repo, &target, &current, cx)
        });
        cx.spawn(async move |this, cx| {
            let commits = commits.await;
            this.update(cx, |this, cx| {
                match commits {
                    Ok(commits) => {
                        this.rows = commit_rows(commits.0.len(), commits.1.len());
                        this.commits = Loading::Ready(commits);
                        this.selected_commit = this
                            .rows
                            .iter()
                            .position(|row| matches!(row, CommitRow::Commit(..)));
                        this.load_commit_files(cx);
                    }
                    Err(err) => this.commits = Loading::Failed(err.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The selected commit.
    fn commit(&self) -> Option<&CommitInfo> {
        let Loading::Ready((ahead, behind)) = &self.commits else {
            return None;
        };
        match self.rows.get(self.selected_commit?)? {
            CommitRow::Commit(true, at) => ahead.get(*at),
            CommitRow::Commit(false, at) => behind.get(*at),
            CommitRow::Header(_) => None,
        }
    }

    /// Reads the selected commit's files, once.
    fn load_commit_files(&mut self, cx: &mut Context<Self>) {
        let Some(oid) = self.commit().map(|commit| commit.oid.clone()) else {
            return;
        };
        if self.commit_files.contains_key(&oid) {
            return;
        }
        self.commit_files.insert(oid.clone(), Loading::Reading);
        let repo = self.repo;
        let read = self
            .git
            .update(cx, |git, cx| git.commit_changes(repo, &oid, cx));
        cx.spawn(async move |this, cx| {
            let files = read.await;
            this.update(cx, |this, cx| {
                let files = match files {
                    Ok(files) => Loading::Ready(files),
                    Err(err) => Loading::Failed(err.to_string()),
                };
                this.commit_files.insert(oid, files);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn selected_commit_files(&self) -> Option<&[Changed]> {
        let oid = &self.commit()?.oid;
        match self.commit_files.get(oid)? {
            Loading::Ready(files) => Some(files),
            _ => None,
        }
    }

    // --- Keys ---

    fn move_selection(&mut self, step: isize, cx: &mut Context<Self>) {
        match self.tab {
            Tab::Files => {
                if let Loading::Ready(files) = &self.files
                    && !files.is_empty()
                {
                    self.selected_file = step_within(self.selected_file, step, files.len());
                    self.file_scroll
                        .scroll_to_item(self.selected_file, ScrollStrategy::Center);
                }
            }
            Tab::Commits if self.in_commit_files => {
                if let Some(files) = self.selected_commit_files()
                    && !files.is_empty()
                {
                    self.selected_commit_file =
                        step_within(self.selected_commit_file, step, files.len());
                }
            }
            Tab::Commits => {
                let commits: Vec<usize> = (0..self.rows.len())
                    .filter(|&index| matches!(self.rows[index], CommitRow::Commit(..)))
                    .collect();
                if commits.is_empty() {
                    return;
                }
                let at = self
                    .selected_commit
                    .and_then(|selected| commits.iter().position(|&row| row == selected))
                    .unwrap_or(0);
                let next = commits[step_within(at, step, commits.len())];
                self.select_commit(next, cx);
            }
        }
        cx.notify();
    }

    fn select_commit(&mut self, row: usize, cx: &mut Context<Self>) {
        self.selected_commit = Some(row);
        self.selected_commit_file = 0;
        self.commit_scroll
            .scroll_to_item(row, ScrollStrategy::Center);
        self.load_commit_files(cx);
        cx.notify();
    }

    fn switch_tab(&mut self, cx: &mut Context<Self>) {
        if self.current.is_none() {
            return;
        }
        self.tab = match self.tab {
            Tab::Commits => Tab::Files,
            Tab::Files => Tab::Commits,
        };
        cx.notify();
    }

    /// ↵: the diff of the selected file (from the commit list — to its files first).
    fn open_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.tab {
            Tab::Files => {
                if let Loading::Ready(files) = &self.files
                    && let Some(file) = files.get(self.selected_file).cloned()
                {
                    self.open_file(&file, window, cx);
                }
            }
            Tab::Commits if !self.in_commit_files => {
                if self
                    .selected_commit_files()
                    .is_some_and(|files| !files.is_empty())
                {
                    self.in_commit_files = true;
                    cx.notify();
                }
            }
            Tab::Commits => {
                let file = self
                    .selected_commit_files()
                    .and_then(|files| files.get(self.selected_commit_file))
                    .cloned();
                if let Some(file) = file {
                    self.open_commit_file(&file, window, cx);
                }
            }
        }
    }

    /// The diff of a file of the Files list: the current branch's version (or the target's) against
    /// the target's (or the working copy).
    fn open_file(&mut self, file: &Changed, window: &mut Window, cx: &mut Context<Self>) {
        let (left, right) = files_sides(&self.target, self.current.as_ref(), file);
        self.dispatch(file, left, right, window, cx);
    }

    /// The diff of a file of a commit: its first parent's version against the commit's.
    fn open_commit_file(&mut self, file: &Changed, window: &mut Window, cx: &mut Context<Self>) {
        let Some(commit) = self.commit().cloned() else {
            return;
        };
        let (left, right) = commit_sides(&commit, file);
        self.dispatch(file, left, right, window, cx);
    }

    fn dispatch(
        &mut self,
        file: &Changed,
        left: DiffSide,
        right: DiffSide,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self
            .git
            .read(cx)
            .repos()
            .get(self.repo)
            .map(|entry| entry.repo.absolute(&file.path))
        else {
            return;
        };
        let action = git::OpenCompareDiff {
            repo: self.repo,
            path,
            left,
            right,
        };
        cx.emit(DismissEvent);
        window.dispatch_action(Box::new(action), cx);
    }

    // --- Rendering ---

    fn title(&self) -> String {
        match &self.current {
            Some((_, label)) => trf("Compare {0} with {1}", &[&self.target, label]),
            None => trf("Diff of {0} with the working tree", &[&self.target]),
        }
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let count = |loading: &Loading<usize>| match loading {
            Loading::Ready(count) => format!(" {count}"),
            _ => String::new(),
        };
        let commits = match &self.commits {
            Loading::Ready((ahead, behind)) => Loading::Ready(ahead.len() + behind.len()),
            _ => Loading::Reading,
        };
        let files = match &self.files {
            Loading::Ready(files) => Loading::Ready(files.len()),
            _ => Loading::Reading,
        };
        let tab = |id: &'static str, label: String, active: bool, which: Tab| {
            div()
                .id(id)
                .h(px(26.))
                .px_2p5()
                .flex()
                .items_center()
                .rounded(px(RADIUS_SM))
                .cursor_pointer()
                .text_size(px(theme::TEXT_SM))
                .when(active, |tab| {
                    tab.bg(ui.accent_soft)
                        .text_color(ui.accent_text)
                        .font_weight(FontWeight::MEDIUM)
                })
                .when(!active, |tab| {
                    tab.text_color(ui.text_muted)
                        .hover(move |style| style.bg(ui.hover).text_color(ui.foreground))
                })
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.tab = which;
                    cx.notify();
                }))
                .child(label)
        };
        div()
            .flex_none()
            .px_3()
            .pt_2()
            .flex()
            .items_center()
            .gap_1()
            .child(tab(
                "compare-commits",
                format!("{}{}", tr("Commits"), count(&commits)),
                self.tab == Tab::Commits,
                Tab::Commits,
            ))
            .child(tab(
                "compare-files",
                format!("{}{}", tr("Files"), count(&files)),
                self.tab == Tab::Files,
                Tab::Files,
            ))
    }

    fn render_commits(&self, focused: bool, cx: &mut Context<Self>) -> AnyElement {
        let ui = Theme::ui(cx);
        let (ahead, behind) = match &self.commits {
            Loading::Reading => return note(tr("Reading…"), ui).into_any_element(),
            Loading::Failed(error) => {
                return note(error.clone(), ui)
                    .text_color(ui.error)
                    .into_any_element();
            }
            Loading::Ready(commits) => commits,
        };
        let now = now_seconds();
        let current = self
            .current
            .as_ref()
            .map(|(_, label)| label.clone())
            .unwrap_or_default();
        let target = self.target.clone();
        let rows = self.rows.clone();
        let ahead = ahead.clone();
        let behind = behind.clone();
        let selected = self.selected_commit;
        let active = focused && !self.in_commit_files;
        uniform_list(
            "compare-commit-rows",
            rows.len(),
            cx.processor(move |_, range: Range<usize>, _, cx| {
                range
                    .map(|index| match rows[index] {
                        CommitRow::Header(mine) => {
                            let (count, label) = match mine {
                                true => {
                                    (ahead.len(), trf("In {0}, not in {1}", &[&target, &current]))
                                }
                                false => (
                                    behind.len(),
                                    trf("In {0}, not in {1}", &[&current, &target]),
                                ),
                            };
                            section_row(label, count, ui).into_any_element()
                        }
                        CommitRow::Commit(mine, at) => {
                            let commit = if mine { &ahead[at] } else { &behind[at] };
                            commit_row(index, commit, selected == Some(index), active, now, ui, cx)
                                .into_any_element()
                        }
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(self.commit_scroll.clone())
        .size_full()
        .into_any_element()
    }

    /// The files of the selected commit, on the right of the commit list.
    fn render_commit_files(
        &self,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let commit = self.commit();
        let content: AnyElement =
            match commit.and_then(|commit| self.commit_files.get(&commit.oid)) {
                None => note(tr("Select a commit to see its files"), ui).into_any_element(),
                Some(Loading::Reading) => note(tr("Reading…"), ui).into_any_element(),
                Some(Loading::Failed(error)) => note(error.clone(), ui)
                    .text_color(ui.error)
                    .into_any_element(),
                Some(Loading::Ready(files)) => div()
                    .flex()
                    .flex_col()
                    .children(files.iter().enumerate().map(|(index, file)| {
                        let selected = self.in_commit_files && index == self.selected_commit_file;
                        let file = file.clone();
                        file_row(("compare-commit-file", index), &file, selected, focused, ui)
                            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                                this.in_commit_files = true;
                                this.selected_commit_file = index;
                                cx.notify();
                                if event.click_count() == 2 {
                                    this.open_commit_file(&file, window, cx);
                                }
                            }))
                    }))
                    .into_any_element(),
            };
        div()
            .id("compare-commit-files")
            .flex_none()
            .w(px(FILES_WIDTH))
            .h_full()
            .border_l_1()
            .border_color(ui.divider)
            .p_1()
            .overflow_y_scroll()
            .child(content)
    }

    fn render_files(&self, focused: bool, cx: &mut Context<Self>) -> AnyElement {
        let ui = Theme::ui(cx);
        let files = match &self.files {
            Loading::Reading => return note(tr("Reading…"), ui).into_any_element(),
            Loading::Failed(error) => {
                return note(error.clone(), ui)
                    .text_color(ui.error)
                    .into_any_element();
            }
            Loading::Ready(files) if files.is_empty() => {
                return div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .child(icon(IconName::Check, ui.dim).size(px(20.)))
                    .child(div().text_color(ui.text_muted).child(tr("No differences")))
                    .into_any_element();
            }
            Loading::Ready(files) => files.clone(),
        };
        let selected = self.selected_file;
        uniform_list(
            "compare-files-list",
            files.len(),
            cx.processor(move |_, range: Range<usize>, _, cx| {
                range
                    .map(|index| {
                        let file = files[index].clone();
                        div()
                            .px_1()
                            .child(
                                file_row(
                                    ("compare-file", index),
                                    &file,
                                    index == selected,
                                    focused,
                                    ui,
                                )
                                .on_click(cx.listener(
                                    move |this, event: &ClickEvent, window, cx| {
                                        this.selected_file = index;
                                        cx.notify();
                                        if event.click_count() == 2 {
                                            this.open_file(&file, window, cx);
                                        }
                                    },
                                )),
                            )
                            .into_any_element()
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(self.file_scroll.clone())
        .size_full()
        .into_any_element()
    }
}

/// The commit list's rows: a header and the commits of each side.
fn commit_rows(ahead: usize, behind: usize) -> Vec<CommitRow> {
    let mut rows = vec![CommitRow::Header(true)];
    rows.extend((0..ahead).map(|at| CommitRow::Commit(true, at)));
    rows.push(CommitRow::Header(false));
    rows.extend((0..behind).map(|at| CommitRow::Commit(false, at)));
    rows
}

/// `at + step`, kept within `0..len`.
fn step_within(at: usize, step: isize, len: usize) -> usize {
    (at as isize + step).clamp(0, len.saturating_sub(1) as isize) as usize
}

/// The sides of a file of the Files list: comparing with the current branch — its version and the
/// target's; with the working tree — the target's and the working copy.
fn files_sides(
    target: &str,
    current: Option<&(String, String)>,
    file: &Changed,
) -> (DiffSide, DiffSide) {
    let old_path = left_path(file);
    match current {
        Some((rev, label)) => (
            DiffSide::Revision {
                rev: rev.clone(),
                path: old_path,
                label: label.clone(),
            },
            DiffSide::Revision {
                rev: target.to_string(),
                path: file.path.clone(),
                label: target.to_string(),
            },
        ),
        None => (
            DiffSide::Revision {
                rev: target.to_string(),
                path: old_path,
                label: target.to_string(),
            },
            DiffSide::WorkingCopy,
        ),
    }
}

/// The sides of a file of a commit: the commit's first parent and the commit.
fn commit_sides(commit: &CommitInfo, file: &Changed) -> (DiffSide, DiffSide) {
    (
        DiffSide::Revision {
            rev: format!("{}^", commit.oid),
            path: left_path(file),
            label: format!("{}^", commit.short),
        },
        DiffSide::Revision {
            rev: commit.oid.clone(),
            path: file.path.clone(),
            label: commit.short.clone(),
        },
    )
}

/// A section's header in the commit list: "In feature/x, not in main · 2".
fn section_row(label: String, count: usize, ui: UiColors) -> impl IntoElement {
    div()
        .h(px(ROW_HEIGHT))
        .px_3()
        .flex()
        .items_center()
        .gap_2()
        .child(ui::section_label(label, ui))
        .child(
            div()
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.dim)
                .child(count.to_string()),
        )
}

/// A commit: a dot, the summary, the short hash, the author and how long ago.
fn commit_row(
    index: usize,
    commit: &CommitInfo,
    selected: bool,
    focused: bool,
    now: i64,
    ui: UiColors,
    cx: &mut Context<CompareDialog>,
) -> impl IntoElement {
    div().px_1().child(
        div()
            .id(("compare-commit", index))
            .h(px(ROW_HEIGHT))
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .rounded(px(RADIUS_SM))
            .cursor_pointer()
            .map(|row| match (selected, focused) {
                (true, true) => row.bg(ui.list_selected),
                (true, false) => row.bg(ui.list_selected_inactive),
                _ => row.hover(move |style| style.bg(ui.hover)),
            })
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.in_commit_files = false;
                this.select_commit(index, cx);
            }))
            .child(div().flex_none().size(px(7.)).rounded(px(4.)).bg(ui.accent))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(commit.summary.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .font_family(theme::code_font())
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.dim)
                    .child(commit.short.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.dim)
                    .child(format!("{}, {}", commit.author, age(now, commit.time))),
            ),
    )
}

/// A file: its icon, its name in the color of its change, its directory dimmed (a rename — where it
/// came from).
fn file_row(
    id: impl Into<gpui::ElementId>,
    file: &Changed,
    selected: bool,
    focused: bool,
    ui: UiColors,
) -> gpui::Stateful<Div> {
    let (dir, name) = match file.path.rsplit_once('/') {
        Some((dir, name)) => (Some(dir.to_string()), name.to_string()),
        None => (None, file.path.clone()),
    };
    let detail: Option<SharedString> = match (file.status, &file.orig_path) {
        (FileStatus::Renamed, Some(orig)) => Some(trf("← {0}", &[orig]).into()),
        _ => dir.map(Into::into),
    };
    div()
        .id(id)
        .h(px(ROW_HEIGHT - 2.))
        .px_2()
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
        .child(file_icon(&name, &ui).render().size(px(14.)))
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_color(git::status_color(file.status, &ui))
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

impl Focusable for CompareDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for CompareDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let focused = self.focus_handle.is_focused(window);
        let with_current = self.current.is_some();
        let body: AnyElement = match self.tab {
            Tab::Commits => div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .pt_1()
                        .child(self.render_commits(focused, cx)),
                )
                .child(self.render_commit_files(focused && self.in_commit_files, cx))
                .into_any_element(),
            Tab::Files => div()
                .flex_1()
                .min_h_0()
                .pt_1()
                .child(self.render_files(focused, cx))
                .into_any_element(),
        };
        let mut hints = vec![("↑↓", tr("navigate")), ("↵", tr("diff"))];
        if with_current {
            hints.push(("⇥", tr("commits / files")));
        }
        hints.push(("esc", tr("close")));
        ui::popover(ui)
            .key_context("CompareDialog")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &FocusFiles, _, cx| {
                if this.tab == Tab::Commits
                    && this
                        .selected_commit_files()
                        .is_some_and(|files| !files.is_empty())
                {
                    this.in_commit_files = true;
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &FocusCommits, _, cx| {
                this.in_commit_files = false;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &SwitchTab, _, cx| this.switch_tab(cx)))
            .on_action(cx.listener(|this, _: &OpenDiff, window, cx| this.open_selected(window, cx)))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(DismissEvent)))
            .w(px(WIDTH))
            .h(px(HEIGHT))
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .h(px(44.))
                    .px_4()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .border_b_1()
                    .border_color(ui.divider)
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(icon(IconName::Diff, ui.text_muted).size(px(15.)))
                            .child(div().min_w_0().truncate().child(self.title())),
                    )
                    .child(
                        ui::icon_button("compare-close", IconName::Close, ui)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    ),
            )
            .when(with_current, |dialog| dialog.child(self.render_tabs(cx)))
            .child(body)
            .child(
                div()
                    .flex_none()
                    .h(px(36.))
                    .px_4()
                    .flex()
                    .items_center()
                    .border_t_1()
                    .border_color(ui.divider)
                    .child(ui::hint_bar(&hints, ui).gap_3()),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sides_follow_what_is_compared() {
        let renamed = Changed {
            status: FileStatus::Renamed,
            path: "new.rs".into(),
            orig_path: Some("old.rs".into()),
        };
        let current = ("main".to_string(), "main".to_string());
        let (left, right) = files_sides("feature/x", Some(&current), &renamed);
        assert_eq!(
            left,
            DiffSide::Revision {
                rev: "main".into(),
                path: "old.rs".into(),
                label: "main".into()
            }
        );
        assert_eq!(
            right,
            DiffSide::Revision {
                rev: "feature/x".into(),
                path: "new.rs".into(),
                label: "feature/x".into()
            }
        );
        let (left, right) = files_sides("feature/x", None, &renamed);
        assert!(matches!(left, DiffSide::Revision { ref rev, .. } if rev == "feature/x"));
        assert_eq!(right, DiffSide::WorkingCopy);
        let commit = CommitInfo {
            oid: "4d7de13a".into(),
            short: "4d7de13".into(),
            summary: String::new(),
            author: String::new(),
            time: 0,
        };
        let (left, right) = commit_sides(&commit, &renamed);
        assert!(
            matches!(left, DiffSide::Revision { ref rev, ref path, .. } if rev == "4d7de13a^" && path == "old.rs")
        );
        assert!(matches!(right, DiffSide::Revision { ref rev, .. } if rev == "4d7de13a"));
    }

    #[test]
    fn commit_rows_have_a_header_per_side() {
        assert_eq!(
            commit_rows(2, 1),
            vec![
                CommitRow::Header(true),
                CommitRow::Commit(true, 0),
                CommitRow::Commit(true, 1),
                CommitRow::Header(false),
                CommitRow::Commit(false, 0),
            ]
        );
        assert_eq!(step_within(0, -1, 3), 0);
        assert_eq!(step_within(2, 1, 3), 2);
        assert_eq!(step_within(1, 1, 3), 2);
    }
}
