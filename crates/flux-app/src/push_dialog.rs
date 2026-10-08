//! The push dialog (⇧⌘K), as in JetBrains IDEs: per repository, the branch and where it goes
//! (`main → origin : main`; the target branch can be edited, a branch the remote doesn't have yet
//! is marked New), the commits that would be pushed and the files of the selected one; "Push" (↵),
//! "Force Push…" (with lease, after a question) and "Push tags". The push itself runs in the
//! background: its progress and result are in the status bar, a rejection is shown with git's
//! output.

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use flux_git::{CommitInfo, FileStatus, PushRequest, Remote, Repo};
use gpui::{
    App, ClickEvent, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, KeyBinding, PromptLevel, Render, SharedString, Subscription, Task, Window, actions,
    div, prelude::*, px,
};

use crate::git::{self, CheckState, GitEvent, GitStore};
use crate::i18n::{tr, trf};
use crate::icons::{IconName, file_icon, icon};
use crate::input::{InputEvent, TextInput};
use crate::theme::{self, Theme};
use crate::ui::{self, RADIUS_SM};
use crate::workspace::Workspace;

actions!(
    push_dialog,
    [Confirm, Dismiss, SelectNext, SelectPrevious, ConfirmTarget]
);

const WIDTH: f32 = 820.;
const HEIGHT: f32 = 500.;
const FILES_WIDTH: f32 = 300.;
const ROW_HEIGHT: f32 = 28.;
/// After the target branch is edited, the outgoing commits are read again after this pause.
const TARGET_DELAY: Duration = Duration::from_millis(300);

pub fn init(cx: &mut App) {
    let dialog = Some("PushDialog");
    cx.bind_keys([
        KeyBinding::new("enter", Confirm, dialog),
        KeyBinding::new("cmd-enter", Confirm, dialog),
        KeyBinding::new("escape", Dismiss, dialog),
        KeyBinding::new("down", SelectNext, dialog),
        KeyBinding::new("up", SelectPrevious, dialog),
        // In the target branch field, ↵ only confirms the name.
        KeyBinding::new("enter", ConfirmTarget, Some("PushTarget")),
    ]);
}

/// Opens the dialog for the repositories of the window (or closes it, when open).
pub fn open(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let git = workspace.git().clone();
    if git.read(cx).repos().is_empty() {
        let message = tr("No Git repository");
        return git.update(cx, |git, cx| {
            git.report(GitEvent::Message(message.into()), cx)
        });
    }
    workspace.toggle_modal(window, cx, move |window, cx| {
        PushDialog::new(git, window, cx)
    });
}

/// A repository in the dialog.
struct PushRepo {
    /// Index in `GitStore::repos`.
    index: usize,
    repo: Repo,
    name: String,
    /// The local branch; `None` — detached HEAD.
    branch: Option<String>,
    /// `origin/main`, if the branch has one.
    upstream: Option<String>,
    remotes: Vec<Remote>,
    remote: Option<usize>,
    target: Entity<TextInput>,
    /// The remote has the target branch.
    exists: bool,
    commits: Vec<CommitInfo>,
    /// Pushed with the others.
    checked: bool,
    loading: bool,
    error: Option<String>,
    reload: Option<Task<()>>,
}

impl PushRepo {
    fn remote_name(&self) -> Option<&str> {
        self.remote
            .and_then(|index| self.remotes.get(index))
            .map(|remote| remote.name.as_str())
    }

    /// Something can go: a branch, a remote, a target, and commits or a new branch.
    fn pushable(&self, cx: &App) -> bool {
        self.branch.is_some()
            && self.remote.is_some()
            && !self.target.read(cx).text().trim().is_empty()
            && (!self.commits.is_empty() || !self.exists)
    }
}

/// What the dialog reads about a repository in the background.
struct Loaded {
    remotes: Vec<Remote>,
    remote: Option<usize>,
    target: String,
    exists: bool,
    commits: Vec<CommitInfo>,
    error: Option<String>,
}

pub struct PushDialog {
    git: Entity<GitStore>,
    focus_handle: FocusHandle,
    repos: Vec<PushRepo>,
    /// The selected commit: index in `repos`, index in its commits.
    selected: Option<(usize, usize)>,
    /// Files of commits, by hash, read when a commit is selected.
    files: HashMap<String, Vec<(FileStatus, String)>>,
    tags: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DismissEvent> for PushDialog {}

impl PushDialog {
    fn new(git: Entity<GitStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut repos = Vec::new();
        let mut subscriptions = Vec::new();
        let entries: Vec<(Repo, Option<String>, Option<String>)> = git
            .read(cx)
            .repos()
            .iter()
            .map(|entry| {
                let branch = &entry.status.branch;
                (
                    entry.repo.clone(),
                    branch.head.clone(),
                    branch.upstream.clone(),
                )
            })
            .collect();
        for (index, (repo, branch, upstream)) in entries.into_iter().enumerate() {
            let name = repo
                .work_dir
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let target = cx.new(|cx| TextInput::new(tr("branch"), cx).compact().code());
            let slot = repos.len();
            subscriptions.push(
                cx.subscribe(&target, move |this, _, event, cx| match event {
                    InputEvent::Changed => this.target_edited(slot, cx),
                }),
            );
            repos.push(PushRepo {
                index,
                repo,
                name,
                branch,
                upstream,
                remotes: Vec::new(),
                remote: None,
                target,
                exists: true,
                commits: Vec::new(),
                checked: false,
                loading: true,
                error: None,
                reload: None,
            });
        }
        let mut dialog = Self {
            git,
            focus_handle: cx.focus_handle(),
            repos,
            selected: None,
            files: HashMap::new(),
            tags: false,
            _subscriptions: subscriptions,
        };
        for slot in 0..dialog.repos.len() {
            dialog.load(slot, None, window, cx);
        }
        dialog
    }

    /// Reads a repository's remotes, the target (the upstream, or a branch of the same name) and
    /// the outgoing commits; `target` — the edited target branch.
    fn load(
        &mut self,
        slot: usize,
        target: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(item) = self.repos.get_mut(slot) else {
            return;
        };
        let Some(branch) = item.branch.clone() else {
            item.loading = false;
            return;
        };
        item.loading = true;
        let repo = item.repo.clone();
        let upstream = item.upstream.clone();
        let chosen = item.remote_name().map(str::to_string);
        let read = cx.background_spawn(async move {
            read_repo(
                &repo,
                &branch,
                upstream.as_deref(),
                chosen.as_deref(),
                target,
            )
        });
        let task = cx.spawn_in(window, async move |this, cx| {
            let loaded = read.await;
            this.update_in(cx, |this, window, cx| this.loaded(slot, loaded, window, cx))
                .ok();
        });
        self.repos[slot].reload = Some(task);
    }

    fn loaded(&mut self, slot: usize, loaded: Loaded, window: &mut Window, cx: &mut Context<Self>) {
        let first_load = self.repos[slot].remotes.is_empty() && self.repos[slot].loading;
        let item = &mut self.repos[slot];
        item.loading = false;
        item.remotes = loaded.remotes;
        item.remote = loaded.remote;
        item.exists = loaded.exists;
        item.commits = loaded.commits;
        item.error = loaded.error;
        let target = item.target.clone();
        if target.read(cx).text() != loaded.target {
            target.update(cx, |input, cx| input.set_text(&loaded.target, cx));
        }
        if first_load {
            let pushable = self.repos[slot].pushable(cx);
            self.repos[slot].checked = pushable;
        }
        if self.selected.is_none_or(|(at, _)| at == slot) {
            self.selected = self
                .repos
                .iter()
                .position(|item| !item.commits.is_empty())
                .map(|at| (at, 0));
            self.load_files(cx);
        }
        let _ = window;
        cx.notify();
    }

    /// The target branch was edited: the outgoing commits are read again after a pause.
    fn target_edited(&mut self, slot: usize, cx: &mut Context<Self>) {
        let Some(item) = self.repos.get(slot) else {
            return;
        };
        if item.loading {
            return;
        }
        let target = item.target.read(cx).text();
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TARGET_DELAY).await;
            this.update(cx, |this, cx| {
                let Some(item) = this.repos.get_mut(slot) else {
                    return;
                };
                let Some(branch) = item.branch.clone() else {
                    return;
                };
                let repo = item.repo.clone();
                let remote = item.remote_name().map(str::to_string);
                let target = target.clone();
                let read = cx.background_spawn(async move {
                    read_repo(&repo, &branch, None, remote.as_deref(), Some(target))
                });
                let reload = cx.spawn(async move |this, cx| {
                    let loaded = read.await;
                    this.update(cx, |this, cx| {
                        if let Some(item) = this.repos.get_mut(slot) {
                            item.exists = loaded.exists;
                            item.commits = loaded.commits;
                            item.error = loaded.error;
                            item.checked = item.pushable(cx) && item.checked;
                        }
                        cx.notify();
                    })
                    .ok();
                });
                item.reload = Some(reload);
            })
            .ok();
        });
        self.repos[slot].reload = Some(task);
    }

    /// The next remote of a repository (a click on the remote's name).
    fn next_remote(&mut self, slot: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.repos.get_mut(slot) else {
            return;
        };
        if item.remotes.len() < 2 {
            return;
        }
        item.remote = Some(item.remote.map_or(0, |at| (at + 1) % item.remotes.len()));
        let target = item.target.read(cx).text();
        self.load(slot, Some(target), window, cx);
    }

    fn load_files(&mut self, cx: &mut Context<Self>) {
        let Some((slot, at)) = self.selected else {
            return;
        };
        let Some(commit) = self.repos.get(slot).and_then(|item| item.commits.get(at)) else {
            return;
        };
        if self.files.contains_key(&commit.oid) {
            return;
        }
        let oid = commit.oid.clone();
        let repo = self.repos[slot].repo.clone();
        let read = cx.background_spawn({
            let oid = oid.clone();
            async move { flux_git::commit_files(&repo, &oid).unwrap_or_default() }
        });
        cx.spawn(async move |this, cx| {
            let files = read.await;
            this.update(cx, |this, cx| {
                this.files.insert(oid, files);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The commits in order, across repositories.
    fn commit_slots(&self) -> Vec<(usize, usize)> {
        self.repos
            .iter()
            .enumerate()
            .flat_map(|(slot, item)| (0..item.commits.len()).map(move |at| (slot, at)))
            .collect()
    }

    fn move_selection(&mut self, step: isize, cx: &mut Context<Self>) {
        let slots = self.commit_slots();
        if slots.is_empty() {
            return;
        }
        let current = self
            .selected
            .and_then(|selected| slots.iter().position(|slot| *slot == selected));
        let next = match current {
            Some(at) => (at as isize + step).clamp(0, slots.len() as isize - 1) as usize,
            None => 0,
        };
        self.selected = Some(slots[next]);
        self.load_files(cx);
        cx.notify();
    }

    /// Pushes the checked repositories; `force` — with lease, after a question.
    fn push(&mut self, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        let requests: Vec<(usize, PushRequest)> = self
            .repos
            .iter()
            .filter(|item| item.checked && item.pushable(cx))
            .filter_map(|item| {
                let branch = item.branch.clone()?;
                let remote = item.remote_name()?.to_string();
                let target = item.target.read(cx).text().trim().to_string();
                let upstream = format!("{remote}/{target}");
                Some((
                    item.index,
                    PushRequest {
                        remote,
                        local_branch: branch,
                        remote_branch: target,
                        // A branch without an upstream (or pushed elsewhere) tracks its target.
                        set_upstream: item.upstream.as_deref() != Some(upstream.as_str()),
                        force_with_lease: force,
                        tags: self.tags,
                    },
                ))
            })
            .collect();
        if requests.is_empty() {
            return;
        }
        if force {
            let names: Vec<String> = requests
                .iter()
                .map(|(_, request)| {
                    format!(
                        "{} → {}/{}",
                        request.local_branch, request.remote, request.remote_branch
                    )
                })
                .collect();
            let answer = window.prompt(
                PromptLevel::Warning,
                tr("Force push?"),
                Some(&format!(
                    "{}\n\n{}",
                    names.join("\n"),
                    tr("The remote branch is overwritten with yours, unless someone pushed to it since your last fetch (--force-with-lease).")
                )),
                &[tr("Force Push"), tr("Cancel")],
                cx,
            );
            cx.spawn_in(window, async move |this, cx| {
                if answer.await == Ok(0) {
                    this.update(cx, |this, cx| this.start(requests, cx)).ok();
                }
            })
            .detach();
            return;
        }
        self.start(requests, cx);
    }

    /// Hands the pushes to the git hub and closes: progress and the result go to the status bar.
    fn start(&mut self, requests: Vec<(usize, PushRequest)>, cx: &mut Context<Self>) {
        for (repo, request) in requests {
            let git = self.git.clone();
            let task = git.update(cx, |git, cx| git.push(repo, request, cx));
            cx.spawn(async move |_, cx| {
                let result = task.await;
                let event = match result {
                    Ok(result) if result.up_to_date => {
                        GitEvent::Message(tr("Everything is up to date").into())
                    }
                    Ok(result) => GitEvent::Message(trf("Pushed: {0}", &[&result.summary]).into()),
                    Err(err) => GitEvent::Error {
                        message: trf("Push failed: {0}", &[&err]).into(),
                        details: err.details().map(str::to_string),
                    },
                };
                git.update(cx, |git, cx| git.report(event, cx)).ok();
            })
            .detach();
        }
        cx.emit(DismissEvent);
    }

    fn can_push(&self, cx: &App) -> bool {
        self.repos
            .iter()
            .any(|item| item.checked && item.pushable(cx))
    }

    // --- Rendering ---

    fn render_repo(&self, slot: usize, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let item = &self.repos[slot];
        let several = self.repos.len() > 1;
        let pushable = item.pushable(cx);
        let state = if item.checked && pushable {
            CheckState::Checked
        } else {
            CheckState::Unchecked
        };
        let head = div()
            .flex_none()
            .h(px(ROW_HEIGHT + 4.))
            .px_2()
            .flex()
            .items_center()
            .gap_1p5()
            .when(several && pushable, |row| {
                row.child(
                    ui::checkbox(("push-repo", slot), state, ui).on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            if let Some(item) = this.repos.get_mut(slot) {
                                item.checked = !item.checked;
                            }
                            cx.notify();
                        },
                    )),
                )
            })
            .when(several, |row| {
                row.child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(item.name.clone()),
                )
            })
            .child(icon(IconName::Branch, ui.violet).size(px(13.)))
            .child(
                div().text_color(ui.violet).child(
                    item.branch
                        .clone()
                        .unwrap_or_else(|| tr("detached HEAD").into()),
                ),
            );
        let head = match (&item.branch, item.remote_name()) {
            (None, _) => head,
            (Some(_), None) if !item.loading => head,
            (Some(_), _) => {
                let remote = item.remote_name().unwrap_or("…").to_string();
                let several_remotes = item.remotes.len() > 1;
                head.child(icon(IconName::ArrowRight, ui.dim).size(px(12.)))
                    .child(
                        div()
                            .id(("push-remote", slot))
                            .px_1()
                            .rounded(px(RADIUS_SM))
                            .text_color(ui.text_muted)
                            .when(several_remotes, |chip| {
                                chip.cursor_pointer()
                                    .hover(move |style| style.bg(ui.hover))
                                    .tooltip(ui::tooltip(tr("Another remote"), None))
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, window, cx| {
                                            this.next_remote(slot, window, cx)
                                        },
                                    ))
                            })
                            .child(remote),
                    )
                    .child(div().text_color(ui.dim).child(":"))
                    .child(
                        div()
                            .key_context("PushTarget")
                            .w(px(180.))
                            .on_action(cx.listener(|this, _: &ConfirmTarget, window, _| {
                                window.focus(&this.focus_handle)
                            }))
                            .child(item.target.clone()),
                    )
                    .when(!item.exists && !item.loading, |row| {
                        row.child(ui::badge(tr("New"), ui.green))
                    })
            }
        };
        let body: gpui::AnyElement = if item.loading {
            note(tr("Reading…"), ui).into_any_element()
        } else if item.branch.is_none() {
            note(tr("HEAD is detached: check out a branch to push"), ui).into_any_element()
        } else if item.remotes.is_empty() {
            note(tr("No remotes: add one with git remote add"), ui).into_any_element()
        } else if let Some(error) = &item.error {
            note(error.clone(), ui)
                .text_color(ui.error)
                .into_any_element()
        } else if item.commits.is_empty() && item.exists {
            note(tr("Nothing to push"), ui).into_any_element()
        } else if item.commits.is_empty() {
            note(tr("A new branch without commits of its own"), ui).into_any_element()
        } else {
            let now = now_seconds();
            div()
                .flex()
                .flex_col()
                .children(item.commits.iter().enumerate().map(|(at, commit)| {
                    let selected = self.selected == Some((slot, at));
                    div()
                        .id(("push-commit", slot * 10_000 + at))
                        .h(px(ROW_HEIGHT))
                        .mx_1()
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .rounded(px(RADIUS_SM))
                        .cursor_pointer()
                        .when(selected, |row| row.bg(ui.list_selected))
                        .when(!selected, |row| row.hover(move |style| style.bg(ui.hover)))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.selected = Some((slot, at));
                            this.load_files(cx);
                            cx.notify();
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
                        )
                }))
                .into_any_element()
        };
        div().flex().flex_col().pb_2().child(head).child(body)
    }

    fn render_files(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let commit = self
            .selected
            .and_then(|(slot, at)| self.repos.get(slot)?.commits.get(at));
        let files = commit.and_then(|commit| self.files.get(&commit.oid));
        let content: gpui::AnyElement = match (commit, files) {
            (None, _) => note(tr("Select a commit to see its files"), ui).into_any_element(),
            (Some(_), None) => note(tr("Reading…"), ui).into_any_element(),
            (Some(_), Some(files)) => div()
                .flex()
                .flex_col()
                .children(files.iter().map(|(status, path)| {
                    let (dir, name) = match path.rsplit_once('/') {
                        Some((dir, name)) => (Some(dir.to_string()), name.to_string()),
                        None => (None, path.clone()),
                    };
                    div()
                        .h(px(ROW_HEIGHT - 2.))
                        .px_3()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .whitespace_nowrap()
                        .child(file_icon(&name, &ui).render().size(px(14.)))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(git::status_color(*status, &ui))
                                .when(*status == FileStatus::Deleted, |name| name.line_through())
                                .child(name),
                        )
                        .children(dir.map(|dir| {
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(px(theme::TEXT_SM))
                                .text_color(ui.dim)
                                .child(dir)
                        }))
                }))
                .into_any_element(),
        };
        div()
            .id("push-files")
            .flex_none()
            .w(px(FILES_WIDTH))
            .h_full()
            .border_l_1()
            .border_color(ui.divider)
            .pt_2()
            .overflow_y_scroll()
            .child(content)
    }
}

/// A quiet line of text in place of a list.
fn note(text: impl Into<SharedString>, ui: crate::theme::UiColors) -> gpui::Div {
    div()
        .px_3()
        .py_1p5()
        .text_size(px(theme::TEXT_SM))
        .text_color(ui.dim)
        .child(text.into())
}

/// Reads a repository for the dialog: remotes, the chosen remote (asked for, otherwise the
/// upstream's, otherwise `origin`, otherwise the first), the target branch (asked for, otherwise
/// the upstream's, otherwise the local branch's name), whether the remote has it, and the commits.
fn read_repo(
    repo: &Repo,
    branch: &str,
    upstream: Option<&str>,
    chosen: Option<&str>,
    target: Option<String>,
) -> Loaded {
    let remotes = match flux_git::remotes(repo) {
        Ok(remotes) => remotes,
        Err(err) => {
            return Loaded {
                remotes: Vec::new(),
                remote: None,
                target: target.unwrap_or_else(|| branch.to_string()),
                exists: true,
                commits: Vec::new(),
                error: Some(err.to_string()),
            };
        }
    };
    let (upstream_remote, upstream_branch) = upstream
        .and_then(|upstream| split_upstream(upstream, &remotes))
        .unzip();
    let remote = chosen
        .and_then(|name| remotes.iter().position(|remote| remote.name == name))
        .or_else(|| {
            upstream_remote
                .as_deref()
                .and_then(|name| remotes.iter().position(|remote| remote.name == name))
        })
        .or_else(|| remotes.iter().position(|remote| remote.name == "origin"))
        .or((!remotes.is_empty()).then_some(0));
    let target = target
        .or(upstream_branch)
        .unwrap_or_else(|| branch.to_string());
    let Some(name) = remote.map(|at| remotes[at].name.clone()) else {
        return Loaded {
            remotes,
            remote: None,
            target,
            exists: true,
            commits: Vec::new(),
            error: None,
        };
    };
    let exists = repo
        .git()
        .read_only()
        .args([
            "rev-parse",
            "--verify",
            "-q",
            &format!("refs/remotes/{name}/{}", target.trim()),
        ])
        .output()
        .is_ok();
    let (commits, error) = match flux_git::outgoing(repo, &name, target.trim()) {
        Ok(commits) => (commits, None),
        Err(err) => (Vec::new(), Some(err.to_string())),
    };
    Loaded {
        remotes,
        remote,
        target,
        exists,
        commits,
        error,
    }
}

/// `origin/feature/x` → (`origin`, `feature/x`), by the remotes that exist (a remote's name may
/// not contain `/`, but a branch's may).
fn split_upstream(upstream: &str, remotes: &[Remote]) -> Option<(String, String)> {
    remotes.iter().find_map(|remote| {
        let branch = upstream.strip_prefix(&remote.name)?.strip_prefix('/')?;
        Some((remote.name.clone(), branch.to_string()))
    })
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

impl Focusable for PushDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PushDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let can_push = self.can_push(cx);
        let repos: Vec<_> = (0..self.repos.len())
            .map(|slot| self.render_repo(slot, cx).into_any_element())
            .collect();
        let push_keys = ui::shortcut_for(&Confirm, window);
        ui::popover(ui)
            .key_context("PushDialog")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &Confirm, window, cx| this.push(false, window, cx)))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(DismissEvent)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.move_selection(-1, cx)))
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
                    .border_b_1()
                    .border_color(ui.divider)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(icon(IconName::Push, ui.text_muted).size(px(15.)))
                            .child(tr("Push Commits")),
                    )
                    .child(
                        ui::icon_button("push-close", IconName::Close, ui)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(
                        div()
                            .id("push-commits")
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .pt_2()
                            .overflow_y_scroll()
                            .children(repos),
                    )
                    .child(self.render_files(cx)),
            )
            .child(
                div()
                    .flex_none()
                    .h(px(52.))
                    .px_4()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_t_1()
                    .border_color(ui.divider)
                    .child(
                        div()
                            .id("push-tags")
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .cursor_pointer()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.text_muted)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.tags = !this.tags;
                                cx.notify();
                            }))
                            .child(ui::checkbox(
                                "push-tags-check",
                                if self.tags {
                                    CheckState::Checked
                                } else {
                                    CheckState::Unchecked
                                },
                                ui,
                            ))
                            .child(tr("Push tags")),
                    )
                    .child(div().flex_1())
                    .child(
                        ui::text_button("push-cancel", tr("Cancel"), false, ui)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    )
                    .child(
                        ui::text_button("push-force", tr("Force Push…"), true, ui)
                            .when(!can_push, |button| button.opacity(0.5))
                            .when(can_push, |button| {
                                button.on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.push(true, window, cx)
                                }))
                            }),
                    )
                    .child(
                        ui::primary_button("push-button", tr("Push"), can_push, ui)
                            .tooltip(ui::tooltip(tr("Push"), push_keys))
                            .when(can_push, |button| {
                                button.on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.push(false, window, cx)
                                }))
                            }),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(name: &str) -> Remote {
        Remote {
            name: name.into(),
            url: String::new(),
        }
    }

    #[test]
    fn upstreams_split_by_the_remotes_that_exist() {
        let remotes = vec![remote("origin"), remote("my-fork")];
        assert_eq!(
            split_upstream("origin/feature/x", &remotes),
            Some(("origin".into(), "feature/x".into()))
        );
        assert_eq!(
            split_upstream("my-fork/main", &remotes),
            Some(("my-fork".into(), "main".into()))
        );
        assert_eq!(split_upstream("gone/main", &remotes), None);
    }

    #[test]
    fn ages_are_short() {
        assert_eq!(age(100, 90), "just now");
        assert_eq!(age(10_000, 10_000 - 300), "5 min");
        assert_eq!(age(100_000, 100_000 - 7200), "2 h");
        assert_eq!(age(1_000_000, 1_000_000 - 3 * 86_400), "3 d");
    }
}
