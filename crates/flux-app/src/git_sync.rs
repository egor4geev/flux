//! Keeping up with the remotes, as JetBrains IDEs do: Update Project (⌘T outside a terminal; the
//! first time it asks Merge or Rebase, with "Don't show again" — the choice is in Settings), Pull…
//! (a dialog: remote, branch, merge / rebase / fast-forward only), Fetch, Update of a branch that
//! isn't checked out, Pull into the current branch from the branches popup, Push… of a chosen
//! branch. Progress is in the status bar (the git hub shows it), the result in a notification;
//! conflicts open the Conflicts dialog.

use std::sync::Arc;

use flux_git::{FetchResult, GitError, Outcome, PullMode, Refs, UpdateMethod, UpdateResult};
use gpui::{
    App, AsyncWindowContext, ClickEvent, Context, DismissEvent, Div, Entity, EventEmitter,
    FocusHandle, Focusable, FontWeight, KeyBinding, Render, ScrollHandle, Subscription, WeakEntity,
    Window, actions, div, prelude::*, px,
};

use crate::git::{self, GitEvent, GitStore};
use crate::i18n::{tr, trf, trn};
use crate::icons::{IconName, icon};
use crate::input::{InputEvent, TextInput};
use crate::notifications::Notification;
use crate::settings::{self, UpdatePreference};
use crate::theme::{self, Theme};
use crate::ui::{self, CheckState, RADIUS_SM};
use crate::workspace::Workspace;

actions!(git_sync, [Confirm, Dismiss, SelectNext, SelectPrevious]);

/// The Update Project question and the Pull dialog.
const UPDATE_WIDTH: f32 = 520.;
const PULL_WIDTH: f32 = 520.;
/// Rows of the Pull dialog's list of remote branches.
const BRANCH_ROW_HEIGHT: f32 = 26.;
const BRANCH_ROWS: usize = 7;
const MIN_BRANCH_ROWS: usize = 3;
/// A notification lists at most this many lines (refs fetched, repositories updated).
const MAX_SUMMARY_LINES: usize = 6;

pub fn init(cx: &mut App) {
    let update = Some("UpdateDialog");
    let pull = Some("PullDialog");
    cx.bind_keys([
        KeyBinding::new("enter", Confirm, update),
        KeyBinding::new("escape", Dismiss, update),
        KeyBinding::new("down", SelectNext, update),
        KeyBinding::new("up", SelectPrevious, update),
        KeyBinding::new("enter", Confirm, pull),
        KeyBinding::new("cmd-enter", Confirm, pull),
        KeyBinding::new("escape", Dismiss, pull),
        KeyBinding::new("down", SelectNext, pull),
        KeyBinding::new("up", SelectPrevious, pull),
    ]);
}

// --- Update Project ---

/// Update Project for `repos` (`None` — every repository): the method from Settings, or the
/// question the first time.
pub fn update_project(
    workspace: &mut Workspace,
    repos: Option<Vec<usize>>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let git = workspace.git().clone();
    let count = git.read(cx).repos().len();
    if count == 0 {
        let message = tr("No Git repository");
        return git.update(cx, |git, cx| {
            git.report(GitEvent::Message(message.into()), cx)
        });
    }
    let repos = repos.unwrap_or_else(|| (0..count).collect());
    let method = match settings::update_method(cx) {
        UpdatePreference::Merge => UpdateMethod::Merge,
        UpdatePreference::Rebase => UpdateMethod::Rebase,
        UpdatePreference::Ask => {
            let this = cx.weak_entity();
            workspace.toggle_modal(window, cx, move |_, cx| UpdateDialog::new(this, repos, cx));
            return;
        }
    };
    run_update(workspace, repos, method, window, cx);
}

/// Updates the repositories one after another in the background (each one's progress in the
/// status bar), then tells what came in.
fn run_update(
    workspace: &mut Workspace,
    repos: Vec<usize>,
    method: UpdateMethod,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let git = workspace.git().clone();
    let tasks: Vec<_> = repos
        .iter()
        .map(|&repo| {
            let name = git.read(cx).repo_name(repo);
            (name, git.update(cx, |git, cx| git.update(repo, method, cx)))
        })
        .collect();
    let several = git.read(cx).repos().len() > 1;
    cx.spawn_in(window, async move |this, cx| {
        let mut results = Vec::new();
        for (name, task) in tasks {
            results.push((name, task.await));
        }
        let summary = UpdateSummary::new(&results, several);
        report_update(&this, &git, summary, results, cx);
    })
    .detach();
}

/// What Update Project did, put together for one notification.
#[derive(Debug, Default, PartialEq)]
struct UpdateSummary {
    /// One line per repository that got something, or that has something to say.
    lines: Vec<String>,
    updated: bool,
    conflicts: bool,
    /// Repositories without an upstream (and nothing else happened).
    no_upstream: Vec<String>,
}

impl UpdateSummary {
    fn new(results: &[(String, Result<UpdateResult, GitError>)], several: bool) -> Self {
        let mut summary = UpdateSummary::default();
        for (name, result) in results {
            let line = match result {
                Ok(result) if result.outcome == Outcome::Conflicts => {
                    summary.conflicts = true;
                    several.then(|| tr("stopped on conflicts").to_string())
                }
                Ok(result) if result.outcome == Outcome::UpToDate || result.commits == 0 => {
                    several.then(|| tr("up to date").to_string())
                }
                Ok(result) => {
                    summary.updated = true;
                    Some(updated_line(result.files, result.commits))
                }
                Err(GitError::NoUpstream(branch)) => {
                    summary.no_upstream.push(branch.clone());
                    Some(match several {
                        true => trf("no tracked branch for {0}", &[branch]),
                        false => trf("No tracked branch for {0}", &[branch]),
                    })
                }
                // Errors get notifications of their own.
                Err(_) => None,
            };
            if let Some(line) = line {
                summary.lines.push(match several {
                    true => format!("{name}: {line}"),
                    false => line,
                });
            }
        }
        summary
    }

    /// The notification, or none (only errors happened: they are reported one by one).
    fn notification(&self, any_ok: bool) -> Option<Notification> {
        let body = summary_body(&self.lines);
        if self.conflicts {
            let hint = tr("Resolve the conflicts, then commit or continue");
            let body = match body {
                Some(lines) => format!("{lines}\n{hint}"),
                None => hint.to_string(),
            };
            return Some(
                Notification::warning(tr("Update stopped on conflicts"))
                    .body(body)
                    .action(tr("Resolve…"), git::ResolveConflicts)
                    .action(tr("Abort"), git::AbortOperation),
            );
        }
        if self.updated {
            let mut notification = Notification::success(tr("Project updated"));
            if let Some(body) = body {
                notification = notification.body(body);
            }
            return Some(notification);
        }
        if !self.no_upstream.is_empty() {
            return Some(
                Notification::warning(tr("Nothing to update from"))
                    .body(trf(
                        "{0} — push the branch first, or set its upstream (git branch --set-upstream-to).",
                        &[&body.unwrap_or_default()],
                    ))
                    .transient(),
            );
        }
        any_ok.then(|| Notification::info(tr("All files are up to date")))
    }
}

/// "5 files updated in 3 commits".
fn updated_line(files: u32, commits: u32) -> String {
    format!(
        "{} {}",
        trn(files as usize, "{n} file updated", "{n} files updated"),
        trn(commits as usize, "in {n} commit", "in {n} commits")
    )
}

/// Lines of a notification: at most a few, then "and N more".
fn summary_body(lines: &[String]) -> Option<String> {
    if lines.is_empty() {
        return None;
    }
    let mut shown: Vec<String> = lines.iter().take(MAX_SUMMARY_LINES).cloned().collect();
    if lines.len() > MAX_SUMMARY_LINES {
        shown.push(trf("and {0} more", &[&(lines.len() - MAX_SUMMARY_LINES)]));
    }
    Some(shown.join("\n"))
}

fn report_update(
    this: &WeakEntity<Workspace>,
    git: &Entity<GitStore>,
    summary: UpdateSummary,
    results: Vec<(String, Result<UpdateResult, GitError>)>,
    cx: &mut AsyncWindowContext,
) {
    let several = results.len() > 1;
    let any_ok = results.iter().any(|(_, result)| result.is_ok());
    git.update(cx, |git, cx| {
        for (name, result) in &results {
            if let Err(err) = result
                && !matches!(err, GitError::NoUpstream(_))
            {
                let title = match several {
                    true => trf("Update of {0} failed", &[name]),
                    false => tr("Update failed").to_string(),
                };
                git.notify_error(&title, err, cx);
            }
        }
        if let Some(notification) = summary.notification(any_ok) {
            git.notify(notification, cx);
        }
    })
    .ok();
    if summary.conflicts {
        show_conflicts(this, cx);
    }
}

/// The Conflicts dialog, after an operation stopped on conflicts.
fn show_conflicts(this: &WeakEntity<Workspace>, cx: &mut AsyncWindowContext) {
    this.update_in(cx, |workspace, window, cx| {
        crate::conflicts_dialog::show_conflicts(workspace, window, cx)
    })
    .ok();
}

/// JetBrains' Update Project question: Merge or Rebase, and "Don't show again".
pub struct UpdateDialog {
    workspace: WeakEntity<Workspace>,
    repos: Vec<usize>,
    rebase: bool,
    remember: bool,
    focus_handle: FocusHandle,
}

impl EventEmitter<DismissEvent> for UpdateDialog {}

impl UpdateDialog {
    fn new(workspace: WeakEntity<Workspace>, repos: Vec<usize>, cx: &mut Context<Self>) -> Self {
        Self {
            workspace,
            repos,
            rebase: false,
            remember: false,
            focus_handle: cx.focus_handle(),
        }
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let method = if self.rebase {
            UpdateMethod::Rebase
        } else {
            UpdateMethod::Merge
        };
        if self.remember {
            let preference = match method {
                UpdateMethod::Merge => UpdatePreference::Merge,
                UpdateMethod::Rebase => UpdatePreference::Rebase,
            };
            settings::set_update_method(preference, cx);
        }
        cx.emit(DismissEvent);
        let repos = std::mem::take(&mut self.repos);
        self.workspace
            .update(cx, |workspace, cx| {
                run_update(workspace, repos, method, window, cx)
            })
            .ok();
    }

    fn option(
        &self,
        id: &'static str,
        rebase: bool,
        title: &'static str,
        detail: &'static str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let selected = self.rebase == rebase;
        div()
            .id(id)
            .flex()
            .gap_2p5()
            .px_2()
            .py_1p5()
            .rounded(px(ui::RADIUS_MD))
            .cursor_pointer()
            .hover(move |style| style.bg(ui.hover))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.rebase = rebase;
                cx.notify();
            }))
            .child(
                div()
                    .pt(px(2.))
                    .child(ui::radio((id, 0usize), selected, ui)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(title)
                    .child(
                        div()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.dim)
                            .child(detail),
                    ),
            )
    }
}

impl Focusable for UpdateDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for UpdateDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let keys = ui::shortcut_in(&Confirm, &self.focus_handle, window);
        ui::popover(ui)
            .key_context("UpdateDialog")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &Confirm, window, cx| this.confirm(window, cx)))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(DismissEvent)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| {
                this.rebase = true;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| {
                this.rebase = false;
                cx.notify();
            }))
            .w(px(UPDATE_WIDTH))
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(icon(IconName::Update, ui.text_muted).size(px(15.)))
                    .child(tr("Update Project")),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .px_2()
                            .pb_1()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.text_muted)
                            .child(tr("How should the incoming commits join your branch?")),
                    )
                    .child(self.option(
                        "update-merge",
                        false,
                        tr("Merge incoming changes into the current branch"),
                        tr("A merge commit joins them, unless your branch can simply move forward"),
                        cx,
                    ))
                    .child(self.option(
                        "update-rebase",
                        true,
                        tr("Rebase the current branch on top of incoming changes"),
                        tr("Your local commits are replayed on top: the history stays a line"),
                        cx,
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id("update-remember")
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .cursor_pointer()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.text_muted)
                            .hover(move |style| style.text_color(ui.foreground))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.remember = !this.remember;
                                cx.notify();
                            }))
                            .child(ui::checkbox(
                                "update-remember-check",
                                CheckState::from_bool(self.remember),
                                ui,
                            ))
                            .child(tr("Don't show again")),
                    )
                    .child(div().flex_1())
                    .child(
                        ui::text_button("update-cancel", tr("Cancel"), false, ui)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    )
                    .child(
                        ui::primary_button("update-ok", tr("Update"), true, ui)
                            .tooltip(ui::tooltip(tr("Update"), keys))
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.confirm(window, cx)
                            })),
                    ),
            )
            .child(
                div()
                    .text_size(px(theme::TEXT_XS))
                    .text_color(ui.dim)
                    .child(tr(
                        "You can change this later in Settings → Version Control",
                    )),
            )
    }
}

// --- Pull ---

/// The Pull dialog for the repository of the active file (or the first).
fn open_pull(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let git = workspace.git().clone();
    if git.read(cx).repos().is_empty() {
        let message = tr("No Git repository");
        return git.update(cx, |git, cx| {
            git.report(GitEvent::Message(message.into()), cx)
        });
    }
    let active = workspace.active_path(cx);
    let repo = git.read(cx).current_repo(active.as_deref()).unwrap_or(0);
    let this = cx.weak_entity();
    workspace.toggle_modal(window, cx, move |window, cx| {
        PullDialog::new(this, git, repo, window, cx)
    });
}

/// "Pull to main": a remote, its branch, how to join it.
pub struct PullDialog {
    workspace: WeakEntity<Workspace>,
    git: Entity<GitStore>,
    repo: usize,
    refs: Arc<Refs>,
    remote: Option<usize>,
    branch: Entity<TextInput>,
    /// The remote's branches matching the field, and the highlighted one.
    matches: Vec<String>,
    highlighted: Option<usize>,
    /// The branch the dialog suggested: while the field holds it, the list shows every branch.
    suggested: String,
    mode: PullMode,
    list_scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DismissEvent> for PullDialog {}

impl PullDialog {
    fn new(
        workspace: WeakEntity<Workspace>,
        git: Entity<GitStore>,
        repo: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let branch = cx.new(|cx| TextInput::new(tr("branch"), cx).code());
        let mut subscriptions =
            vec![
                cx.subscribe_in(&branch, window, |this, _, event, _, cx| match event {
                    InputEvent::Changed => this.filter(cx),
                }),
            ];
        // Branches arrive after a fetch: the list follows the hub.
        subscriptions.push(cx.observe(&git, |this, _, cx| this.refs_changed(cx)));
        git.update(cx, |git, cx| git.reload_refs(repo, cx));
        let mut dialog = Self {
            workspace,
            git,
            repo,
            refs: Arc::default(),
            remote: None,
            branch,
            matches: Vec::new(),
            highlighted: None,
            suggested: String::new(),
            mode: PullMode::Merge,
            list_scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        };
        dialog.select_repo(repo, cx);
        dialog
    }

    /// The repository changed (or the dialog opened): its remotes, the upstream's remote and branch.
    fn select_repo(&mut self, repo: usize, cx: &mut Context<Self>) {
        self.repo = repo;
        self.refs = self.git.read(cx).refs(repo);
        let upstream = self.upstream(cx);
        self.remote = default_remote(
            &self.refs.remotes,
            upstream.as_ref().map(|(r, _)| r.as_str()),
        );
        let branch = upstream
            .map(|(_, branch)| branch)
            .or_else(|| self.git.read(cx).current_branch(repo))
            .unwrap_or_default();
        self.suggested = branch.clone();
        self.branch.update(cx, |input, cx| {
            input.set_text(&branch, cx);
            input.select_all(cx);
        });
        self.filter(cx);
    }

    /// New refs from the hub (the first read, a fetch): the remote is chosen if it wasn't yet.
    fn refs_changed(&mut self, cx: &mut Context<Self>) {
        let refs = self.git.read(cx).refs(self.repo);
        if Arc::ptr_eq(&refs, &self.refs) {
            return;
        }
        let first = self.refs.remotes.is_empty() && !refs.remotes.is_empty();
        self.refs = refs;
        if first {
            let repo = self.repo;
            return self.select_repo(repo, cx);
        }
        self.filter(cx);
    }

    /// The current branch's upstream, as (remote, branch).
    fn upstream(&self, cx: &App) -> Option<(String, String)> {
        let entry = self.git.read(cx).repos().get(self.repo)?;
        let upstream = entry.status.branch.upstream.as_deref()?;
        split_remote_branch(upstream, &self.refs.remotes)
    }

    fn remote_name(&self) -> Option<&str> {
        self.remote
            .and_then(|index| self.refs.remotes.get(index))
            .map(String::as_str)
    }

    /// The remote's branches that contain the field's text (all of them while it holds the
    /// suggested branch).
    fn filter(&mut self, cx: &mut Context<Self>) {
        let query = self.branch.read(cx).text().trim().to_lowercase();
        let filtering = !query.is_empty() && query != self.suggested.to_lowercase();
        let remote = self.remote_name().map(str::to_string);
        self.matches = self
            .refs
            .remote
            .iter()
            .filter(|reference| reference.remote.as_deref() == remote.as_deref())
            .map(|reference| reference.short_name().to_string())
            .filter(|name| !filtering || name.to_lowercase().contains(&query))
            .collect();
        // The exact branch goes first, highlighted: it is what ↵ pulls.
        let exact = self
            .matches
            .iter()
            .position(|name| name.to_lowercase() == query);
        if let Some(at) = exact {
            let name = self.matches.remove(at);
            self.matches.insert(0, name);
        }
        self.highlighted = exact.map(|_| 0);
        cx.notify();
    }

    fn next_repo(&mut self, cx: &mut Context<Self>) {
        let count = self.git.read(cx).repos().len();
        if count > 1 {
            self.select_repo((self.repo + 1) % count, cx);
        }
    }

    fn next_remote(&mut self, cx: &mut Context<Self>) {
        let count = self.refs.remotes.len();
        if count > 1 {
            self.remote = Some(self.remote.map_or(0, |at| (at + 1) % count));
            self.filter(cx);
        }
    }

    fn move_highlight(&mut self, step: isize, cx: &mut Context<Self>) {
        if self.matches.is_empty() {
            return;
        }
        let last = self.matches.len() as isize - 1;
        let next = match self.highlighted {
            Some(at) => (at as isize + step).clamp(0, last),
            None if step > 0 => 0,
            None => last,
        } as usize;
        self.choose(next, cx);
    }

    /// A branch of the list goes into the field.
    fn choose(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(name) = self.matches.get(index).cloned() else {
            return;
        };
        self.highlighted = Some(index);
        self.list_scroll.scroll_to_item(index);
        // Setting the text filters the list again: keep it as it is while choosing.
        let matches = std::mem::take(&mut self.matches);
        self.branch
            .update(cx, |input, cx| input.set_text(&name, cx));
        self.matches = matches;
        self.highlighted = Some(index);
        cx.notify();
    }

    fn can_pull(&self, cx: &App) -> bool {
        self.remote_name().is_some() && !self.branch.read(cx).text().trim().is_empty()
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_pull(cx) {
            return;
        }
        let Some(remote) = self.remote_name().map(str::to_string) else {
            return;
        };
        let branch = self.branch.read(cx).text().trim().to_string();
        let (repo, mode) = (self.repo, self.mode);
        cx.emit(DismissEvent);
        self.workspace
            .update(cx, |workspace, cx| {
                run_pull(workspace, repo, remote, branch, mode, window, cx)
            })
            .ok();
    }

    fn render_branches(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let rows: Vec<_> = self
            .matches
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let selected = self.highlighted == Some(index);
                div()
                    .id(("pull-branch", index))
                    .h(px(BRANCH_ROW_HEIGHT))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .rounded(px(RADIUS_SM))
                    .cursor_pointer()
                    .when(selected, |row| row.bg(ui.list_selected))
                    .when(!selected, |row| row.hover(move |style| style.bg(ui.hover)))
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        this.choose(index, cx);
                        if event.click_count() == 2 {
                            this.confirm(window, cx);
                        }
                    }))
                    .child(icon(IconName::Branch, ui.dim).size(px(12.)))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(theme::code_font())
                            .text_size(px(theme::TEXT_SM))
                            .child(name.clone()),
                    )
            })
            .collect();
        let empty = rows.is_empty();
        let note = if self.refs.remotes.is_empty() {
            tr("No remotes: add one with git remote add")
        } else {
            tr("No such branch on the remote yet — fetch first")
        };
        div()
            .id("pull-branches")
            .h(px(BRANCH_ROW_HEIGHT
                * self.matches.len().clamp(MIN_BRANCH_ROWS, BRANCH_ROWS)
                    as f32
                + 8.))
            .p_1()
            .rounded(px(ui::RADIUS_MD))
            .border_1()
            .border_color(ui.island_border)
            .overflow_y_scroll()
            .track_scroll(&self.list_scroll)
            .when(empty, |list| {
                list.child(
                    div()
                        .px_2()
                        .py_1p5()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.dim)
                        .child(note),
                )
            })
            .children(rows)
    }

    fn render_mode(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let modes = [
            (PullMode::Merge, tr("Merge")),
            (PullMode::Rebase, tr("Rebase")),
            (PullMode::FastForwardOnly, tr("Fast-forward only")),
        ];
        div()
            .flex()
            .items_center()
            .gap_4()
            .children(modes.into_iter().enumerate().map(|(index, (mode, label))| {
                let selected = self.mode == mode;
                div()
                    .id(("pull-mode", index))
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .cursor_pointer()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(if selected {
                        ui.foreground
                    } else {
                        ui.text_muted
                    })
                    .hover(move |style| style.text_color(ui.foreground))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.mode = mode;
                        cx.notify();
                    }))
                    .child(ui::radio(("pull-mode-radio", index), selected, ui))
                    .child(label)
            }))
    }
}

impl Focusable for PullDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.branch.focus_handle(cx)
    }
}

impl Render for PullDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let can_pull = self.can_pull(cx);
        let git = self.git.read(cx);
        let several = git.repos().len() > 1;
        let repo_name = git.repo_name(self.repo);
        let current = git
            .current_branch(self.repo)
            .unwrap_or_else(|| tr("detached HEAD").to_string());
        let remote = self.remote_name().unwrap_or("—").to_string();
        let several_remotes = self.refs.remotes.len() > 1;
        let keys = ui::shortcut_for(&Confirm, window);
        let chip = |id: &'static str, text: String, cycles: bool, tooltip: &'static str| {
            div()
                .id(id)
                .flex_none()
                .h(px(ui::ICON_BUTTON_SIZE))
                .px_2()
                .flex()
                .items_center()
                .gap_1()
                .rounded(px(RADIUS_SM))
                .border_1()
                .border_color(ui.input_border)
                .text_size(px(theme::TEXT_SM))
                .when(cycles, |chip| {
                    chip.cursor_pointer()
                        .hover(move |style| style.bg(ui.hover))
                        .tooltip(ui::tooltip(tooltip, None))
                })
                .child(text)
                .when(cycles, |chip| {
                    chip.child(icon(IconName::ChevronDown, ui.dim).size(px(11.)))
                })
        };
        ui::popover(ui)
            .key_context("PullDialog")
            .on_action(cx.listener(|this, _: &Confirm, window, cx| this.confirm(window, cx)))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(DismissEvent)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_highlight(1, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.move_highlight(-1, cx)))
            .w(px(PULL_WIDTH))
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(icon(IconName::Pull, ui.text_muted).size(px(15.)))
                    .child(trf("Pull to {0}", &[&current]))
                    .child(div().flex_1())
                    .child(
                        ui::icon_button("pull-close", IconName::Close, ui)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(several, |row| {
                        row.child(
                            chip("pull-repo", repo_name, true, tr("Another repository")).on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| this.next_repo(cx)),
                            ),
                        )
                    })
                    .child(
                        chip("pull-remote", remote, several_remotes, tr("Another remote"))
                            .on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| this.next_remote(cx)),
                            ),
                    )
                    .child(div().flex_1().min_w_0().child(self.branch.clone())),
            )
            .child(self.render_branches(cx))
            .child(self.render_mode(cx))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .child(
                        ui::text_button("pull-cancel", tr("Cancel"), false, ui)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    )
                    .child(
                        ui::primary_button("pull-ok", tr("Pull"), can_pull, ui)
                            .tooltip(ui::tooltip(tr("Pull"), keys))
                            .when(can_pull, |button| {
                                button.on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.confirm(window, cx)
                                }))
                            }),
                    ),
            )
    }
}

/// The remote a pull starts with: the upstream's, otherwise `origin`, otherwise the first.
fn default_remote(remotes: &[String], upstream_remote: Option<&str>) -> Option<usize> {
    upstream_remote
        .and_then(|name| remotes.iter().position(|remote| remote == name))
        .or_else(|| remotes.iter().position(|remote| remote == "origin"))
        .or((!remotes.is_empty()).then_some(0))
}

/// `origin/feature/x` → (`origin`, `feature/x`), by the remotes that exist (a remote's name may
/// not contain `/`, a branch's may; the longest remote name wins).
fn split_remote_branch(name: &str, remotes: &[String]) -> Option<(String, String)> {
    remotes
        .iter()
        .filter_map(|remote| {
            let branch = name.strip_prefix(remote.as_str())?.strip_prefix('/')?;
            (!branch.is_empty()).then(|| (remote.clone(), branch.to_string()))
        })
        .max_by_key(|(remote, _)| remote.len())
}

/// Pulls in the background, then reports.
fn run_pull(
    workspace: &mut Workspace,
    repo: usize,
    remote: String,
    branch: String,
    mode: PullMode,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let git = workspace.git().clone();
    let current = git
        .read(cx)
        .current_branch(repo)
        .unwrap_or_else(|| tr("HEAD").to_string());
    let task = git.update(cx, |git, cx| git.pull(repo, &remote, &branch, mode, cx));
    let source = format!("{remote}/{branch}");
    cx.spawn_in(window, async move |this, cx| {
        let result = task.await;
        let conflicts = matches!(result, Ok(Outcome::Conflicts));
        git.update(cx, |git, cx| match &result {
            Ok(outcome) => {
                if let Some(notification) = pull_notification(*outcome, &source, &current) {
                    git.notify(notification, cx);
                }
            }
            Err(err) => git.notify_error(tr("Pull failed"), err, cx),
        })
        .ok();
        if conflicts {
            show_conflicts(&this, cx);
        }
    })
    .detach();
}

/// The notification of a pull's outcome (`source` — "origin/main", `target` — the current branch).
fn pull_notification(outcome: Outcome, source: &str, target: &str) -> Option<Notification> {
    Some(match outcome {
        Outcome::UpToDate => Notification::info(tr("Already up to date"))
            .body(trf("{0} has nothing {1} doesn't have", &[&source, &target])),
        Outcome::FastForward | Outcome::Done => {
            Notification::success(trf("Pulled {0} into {1}", &[&source, &target]))
        }
        Outcome::Conflicts => Notification::warning(tr("Pull stopped on conflicts"))
            .body(tr("Resolve the conflicts, then commit or continue"))
            .action(tr("Resolve…"), git::ResolveConflicts)
            .action(tr("Abort"), git::AbortOperation),
    })
}

/// "Pull into 'main' Using Merge / Rebase" of a remote branch in the branches popup.
fn pull_ref(
    workspace: &mut Workspace,
    action: &git::PullRef,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let git = workspace.git().clone();
    let refs = git.read(cx).refs(action.repo);
    let Some((remote, branch)) = split_remote_branch(&action.name, &refs.remotes) else {
        let message = trf("{0} isn't a branch of a remote", &[&action.name]);
        return git.update(cx, |git, cx| {
            git.report(GitEvent::Message(message.into()), cx)
        });
    };
    let mode = if action.rebase {
        PullMode::Rebase
    } else {
        PullMode::Merge
    };
    run_pull(workspace, action.repo, remote, branch, mode, window, cx);
}

// --- Fetch ---

/// Fetch of every repository; one notification about what came.
fn fetch_all(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let git = workspace.git().clone();
    let count = git.read(cx).repos().len();
    if count == 0 {
        let message = tr("No Git repository");
        return git.update(cx, |git, cx| {
            git.report(GitEvent::Message(message.into()), cx)
        });
    }
    let tasks: Vec<_> = (0..count)
        .map(|repo| {
            let name = git.read(cx).repo_name(repo);
            (name, git.update(cx, |git, cx| git.fetch(repo, cx)))
        })
        .collect();
    cx.spawn_in(window, async move |_, cx| {
        let mut results = Vec::new();
        for (name, task) in tasks {
            results.push((name, task.await));
        }
        let several = results.len() > 1;
        git.update(cx, |git, cx| {
            let mut lines = Vec::new();
            for (name, result) in &results {
                match result {
                    Ok(fetched) => {
                        lines.extend(fetch_lines(fetched).into_iter().map(|line| match several {
                            true => format!("{name}: {line}"),
                            false => line,
                        }))
                    }
                    Err(err) => {
                        let title = match several {
                            true => trf("Fetch of {0} failed", &[name]),
                            false => tr("Fetch failed").to_string(),
                        };
                        git.notify_error(&title, err, cx)
                    }
                }
            }
            let any_ok = results.iter().any(|(_, result)| result.is_ok());
            if let Some(body) = summary_body(&lines) {
                git.notify(Notification::success(tr("Fetched")).body(body), cx);
            } else if any_ok {
                git.notify(
                    Notification::info(tr("Fetch: nothing new on the remotes")),
                    cx,
                );
            }
        })
        .ok();
    })
    .detach();
}

/// What a fetch brought, line by line: "origin/main: +3", "new: origin/feature/y", "gone:
/// origin/old".
fn fetch_lines(fetched: &FetchResult) -> Vec<String> {
    let mut lines: Vec<String> = fetched
        .updated
        .iter()
        .map(|(branch, count)| {
            format!(
                "{branch}: {}",
                trn(*count as usize, "{n} new commit", "{n} new commits")
            )
        })
        .collect();
    lines.extend(
        fetched
            .new_branches
            .iter()
            .map(|branch| trf("new branch {0}", &[branch])),
    );
    lines.extend(
        fetched
            .pruned
            .iter()
            .map(|branch| trf("{0} is gone", &[branch])),
    );
    lines
}

// --- Update of a branch ---

/// Update in the branches popup: the current branch — Update Project for its repository; another
/// one — moved forward to its upstream without a checkout.
fn update_branch(
    workspace: &mut Workspace,
    action: &git::UpdateBranch,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let git = workspace.git().clone();
    if git.read(cx).current_branch(action.repo).as_deref() == Some(action.name.as_str()) {
        return update_project(workspace, Some(vec![action.repo]), window, cx);
    }
    let name = action.name.clone();
    let upstream = git
        .read(cx)
        .refs(action.repo)
        .local(&name)
        .and_then(|branch| branch.upstream.clone());
    let task = git.update(cx, |git, cx| git.fast_forward(action.repo, &name, cx));
    cx.spawn_in(window, async move |_, cx| {
        let result = task.await;
        git.update(cx, |git, cx| {
            let notification = match &result {
                Ok(Outcome::UpToDate) => Notification::info(trf("{0} is up to date", &[&name])),
                Ok(_) => {
                    let mut notification = Notification::success(trf("Updated {0}", &[&name]));
                    if let Some(upstream) = &upstream {
                        notification = notification.body(trf("Fast-forwarded to {0}", &[upstream]));
                    }
                    notification
                }
                Err(GitError::NoUpstream(_)) => {
                    Notification::warning(trf("No tracked branch for {0}", &[&name])).transient()
                }
                Err(err) if is_diverged(err) => {
                    Notification::warning(trf("Can't update {0}", &[&name])).body(tr(
                        "It has commits of its own: check it out and update it (merge or rebase)",
                    ))
                }
                Err(err) => {
                    return git.notify_error(&trf("Update of {0} failed", &[&name]), err, cx);
                }
            };
            git.notify(notification, cx);
        })
        .ok();
    })
    .detach();
}

/// git refused to move a branch because it has diverged ("non-fast-forward", "rejected").
fn is_diverged(error: &GitError) -> bool {
    let text = error.details().unwrap_or_default().to_lowercase();
    text.contains("non-fast-forward") || text.contains("[rejected]") || text.contains("diverged")
}

/// The window-level actions: `UpdateProject`, `Pull`, `Fetch`, `PullRef`, `UpdateBranch`,
/// `PushBranch`.
pub fn workspace_actions(root: Div, cx: &mut Context<Workspace>) -> Div {
    root.on_action(cx.listener(|this, _: &git::UpdateProject, window, cx| {
        update_project(this, None, window, cx)
    }))
    .on_action(cx.listener(|this, _: &git::Pull, window, cx| open_pull(this, window, cx)))
    .on_action(cx.listener(|this, _: &git::Fetch, window, cx| fetch_all(this, window, cx)))
    .on_action(
        cx.listener(|this, action: &git::PullRef, window, cx| pull_ref(this, action, window, cx)),
    )
    .on_action(cx.listener(|this, action: &git::UpdateBranch, window, cx| {
        update_branch(this, action, window, cx)
    }))
    .on_action(cx.listener(|this, action: &git::PushBranch, window, cx| {
        crate::push_dialog::open_for_branch(this, action.repo, action.name.clone(), window, cx)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(outcome: Outcome, commits: u32, files: u32) -> Result<UpdateResult, GitError> {
        Ok(UpdateResult {
            outcome,
            branch: "main".into(),
            upstream: "origin/main".into(),
            commits,
            files,
        })
    }

    #[test]
    fn update_summaries_say_what_came_in() {
        let one = |result| UpdateSummary::new(&[("flux".to_string(), result)], false);
        let updated = one(result(Outcome::Done, 3, 5));
        assert!(updated.updated && !updated.conflicts);
        assert_eq!(updated.lines, vec!["5 files updated in 3 commits"]);
        let up_to_date = one(result(Outcome::UpToDate, 0, 0));
        assert!(up_to_date.lines.is_empty());
        assert_eq!(
            up_to_date
                .notification(true)
                .map(|notification| notification.title.to_string()),
            Some("All files are up to date".to_string())
        );
        let conflicts = one(result(Outcome::Conflicts, 2, 1));
        assert!(conflicts.conflicts && conflicts.lines.is_empty());
        let notification = conflicts.notification(true).unwrap();
        assert_eq!(notification.actions.len(), 2);
        assert_eq!(
            notification.body.as_ref().map(|body| body.to_string()),
            Some("Resolve the conflicts, then commit or continue".to_string())
        );
        let no_upstream = one(Err(GitError::NoUpstream("main".into())));
        assert_eq!(no_upstream.no_upstream, vec!["main".to_string()]);
        assert_eq!(no_upstream.lines, vec!["No tracked branch for main"]);
        // Only an error: the error has its own notification.
        let failed = one(Err(GitError::Canceled));
        assert!(failed.notification(false).is_none());
    }

    #[test]
    fn several_repositories_are_named() {
        let summary = UpdateSummary::new(
            &[
                ("app".to_string(), result(Outcome::FastForward, 1, 1)),
                ("lib".to_string(), result(Outcome::UpToDate, 0, 0)),
            ],
            true,
        );
        assert_eq!(
            summary.lines,
            vec!["app: 1 file updated in 1 commit", "lib: up to date"]
        );
    }

    #[test]
    fn fetch_results_become_lines() {
        let fetched = FetchResult {
            updated: vec![("origin/main".into(), 3)],
            new_branches: vec!["origin/feature/y".into()],
            pruned: vec!["origin/old".into()],
        };
        assert_eq!(
            fetch_lines(&fetched),
            vec![
                "origin/main: 3 new commits",
                "new branch origin/feature/y",
                "origin/old is gone",
            ]
        );
        assert!(fetch_lines(&FetchResult::default()).is_empty());
        let many: Vec<String> = (0..9).map(|n| n.to_string()).collect();
        assert!(summary_body(&many).unwrap().ends_with("and 3 more"));
    }

    #[test]
    fn remote_branches_split_by_the_remotes_that_exist() {
        let remotes = vec!["origin".to_string(), "my-fork".to_string()];
        assert_eq!(
            split_remote_branch("origin/feature/x", &remotes),
            Some(("origin".into(), "feature/x".into()))
        );
        assert_eq!(split_remote_branch("gone/main", &remotes), None);
        assert_eq!(split_remote_branch("origin/", &remotes), None);
        assert_eq!(default_remote(&remotes, Some("my-fork")), Some(1));
        assert_eq!(default_remote(&remotes, None), Some(0));
        assert_eq!(default_remote(&[], None), None);
    }

    #[test]
    fn diverged_branches_are_told_apart() {
        let error = |message: &str| GitError::Failed {
            command: "git fetch".into(),
            message: message.into(),
        };
        assert!(is_diverged(&error(
            " ! [rejected]        main       -> main  (non-fast-forward)"
        )));
        assert!(!is_diverged(&error("fatal: couldn't find remote ref x")));
    }
}
