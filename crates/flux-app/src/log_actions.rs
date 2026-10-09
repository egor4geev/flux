//! Operations on commits of the log (part C of stage 6.3), as JetBrains' log context menu: Copy
//! Revision Number, Cherry-Pick; Checkout Revision (a detached HEAD, with a warning), Compare with
//! Local; Reset Current Branch to Here…, Revert Commit, Undo Commit; the history rewriting entries
//! (part D: Edit Commit Message…, Fixup / Squash Commits, Drop Commits, Interactively Rebase from
//! Here…); New Branch…, New Tag…; Go to Parent Commit.
//!
//! The operations run in the git hub (`impl GitStore` below); the flows here ask (the Reset
//! dialog, the tag name, Undo of a pushed commit) and report in notifications. Cherry-pick and
//! revert that stop on conflicts open the Conflicts dialog, as merges do.

use std::path::PathBuf;

use flux_git::{GitError, Outcome, RepoState, ResetMode};
use gpui::{
    Action, App, ClickEvent, ClipboardItem, Context, DismissEvent, Div, EventEmitter, FocusHandle,
    Focusable, FontWeight, KeyBinding, PromptButton, PromptLevel, Render, Task, WeakEntity, Window,
    actions, div, prelude::*, px,
};

use crate::context_menu::ContextMenu;
use crate::git::{self, GitStore};
use crate::git_log::LogSelection;
use crate::i18n::{tr, trf, trn};
use crate::input_dialog::InputDialog;
use crate::notifications::Notification;
use crate::theme::{self, Theme};
use crate::ui;
use crate::workspace::Workspace;

actions!(reset_dialog, [Confirm, Dismiss, SelectNext, SelectPrevious]);

/// Copy Revision Number: the hashes of the selected commits, one per line.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct CopyRevisionNumber {
    pub oids: Vec<String>,
}

/// Jump to Source of a commit's file (F4 in the commit pane): the file in its tab.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct OpenCommitFile {
    pub path: PathBuf,
}

const RESET_WIDTH: f32 = 480.;

pub fn init(cx: &mut App) {
    let context = Some("ResetDialog");
    cx.bind_keys([
        KeyBinding::new("enter", Confirm, context),
        KeyBinding::new("escape", Dismiss, context),
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("up", SelectPrevious, context),
    ]);
}

/// What applies to the selected commits right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Applicable {
    single: bool,
    /// No merge, rebase, cherry-pick… in progress.
    idle: bool,
    /// HEAD is a branch (rewriting history and reset move it).
    on_branch: bool,
    has_merge: bool,
    /// The (single) commit is HEAD.
    is_head: bool,
    has_parent: bool,
}

fn applicable(selection: &LogSelection, git: &GitStore) -> Applicable {
    let entry = git.repos().get(selection.repo);
    let branch = entry.map(|entry| &entry.status.branch);
    let head = branch.and_then(|branch| branch.oid.clone());
    let first = selection.commits.first();
    Applicable {
        single: selection.commits.len() == 1,
        idle: git.operation(selection.repo).state == RepoState::Normal,
        on_branch: branch.is_some_and(|branch| branch.head.is_some()),
        has_merge: selection.commits.iter().any(|c| c.parents.len() > 1),
        is_head: selection.commits.len() == 1 && first.map(|c| &c.oid) == head.as_ref(),
        has_parent: first.is_some_and(|c| !c.parents.is_empty()),
    }
}

/// The context menu of selected commits; `git` tells what applies (the current branch, HEAD, an
/// operation in progress). Rewriting history needs a branch, no operation in progress and no merge
/// commits; whether the commits are on the current branch is checked by the operation itself.
pub fn commit_menu(menu: ContextMenu, selection: &LogSelection, git: &GitStore) -> ContextMenu {
    let Some(first) = selection.commits.first() else {
        return menu;
    };
    let repo = selection.repo;
    let oid = first.oid.clone();
    let oids: Vec<String> = selection.commits.iter().map(|c| c.oid.clone()).collect();
    let can = applicable(selection, git);
    let rewrite = can.idle && can.on_branch && !can.has_merge;
    let several = selection.commits.len() > 1;
    // The oldest selected commit takes the others (`oids` are newest first).
    let (target, rest) = match oids.split_last() {
        Some((target, rest)) => (target.clone(), rest.to_vec()),
        None => (oid.clone(), Vec::new()),
    };
    menu.entry(
        tr("Copy Revision Number"),
        CopyRevisionNumber { oids: oids.clone() },
    )
    .entry_if(
        can.idle && !can.is_head,
        tr("Cherry-Pick"),
        git::CherryPick {
            repo,
            oids: oids.clone(),
        },
    )
    .separator()
    .entry_if(
        can.single,
        tr("Checkout Revision"),
        git::CheckoutCommit {
            repo,
            oid: oid.clone(),
        },
    )
    .entry_if(
        can.single,
        tr("Compare with Local"),
        git::DiffWithWorkingTree {
            repo,
            name: oid.clone(),
        },
    )
    .separator()
    .entry_if(
        can.single && can.idle,
        tr("Reset Current Branch to Here…"),
        git::ResetToCommit {
            repo,
            oid: oid.clone(),
        },
    )
    .entry_if(
        can.idle && !can.has_merge,
        if several {
            tr("Revert Commits")
        } else {
            tr("Revert Commit")
        },
        git::RevertCommits {
            repo,
            oids: oids.clone(),
        },
    )
    .entry_if(
        can.is_head && can.idle && can.on_branch && !can.has_merge && can.has_parent,
        tr("Undo Commit…"),
        git::UndoCommit {
            repo,
            oid: oid.clone(),
        },
    )
    .separator()
    .entry_if(
        can.single && rewrite,
        tr("Edit Commit Message…"),
        git::EditCommitMessage {
            repo,
            oid: oid.clone(),
        },
    )
    .entry_if(
        several && rewrite,
        tr("Fixup Commits"),
        git::MeldCommits {
            repo,
            target: target.clone(),
            oids: rest.clone(),
            squash: false,
        },
    )
    .entry_if(
        several && rewrite,
        tr("Squash Commits…"),
        git::MeldCommits {
            repo,
            target,
            oids: rest,
            squash: true,
        },
    )
    .entry_if(
        rewrite,
        if several {
            tr("Drop Commits")
        } else {
            tr("Drop Commit")
        },
        git::DropCommits {
            repo,
            oids: oids.clone(),
        },
    )
    .entry_if(
        can.single && rewrite,
        tr("Interactively Rebase from Here…"),
        git::InteractiveRebase {
            repo,
            oid: oid.clone(),
        },
    )
    .separator()
    .entry_if(
        can.single,
        tr("New Branch…"),
        git::NewBranchFrom {
            repo,
            start: oid.clone(),
        },
    )
    .entry_if(
        can.single,
        tr("New Tag…"),
        git::NewTagAt {
            repo,
            oid: oid.clone(),
        },
    )
    .separator()
    .entry_if(
        can.single && can.has_parent,
        tr("Go to Parent Commit"),
        git::ShowCommitInLog {
            repo,
            oid: first.parents.first().cloned().unwrap_or_default(),
        },
    )
}

/// The window-level actions of commits: `CheckoutCommit`, `NewTagAt`, `CherryPick`,
/// `RevertCommits`, `ResetToCommit`, `UndoCommit`, `CopyRevisionNumber`, `OpenCommitFile`.
pub fn workspace_actions(root: Div, cx: &mut Context<Workspace>) -> Div {
    let root = root
        .on_action(
            cx.listener(|this, action: &git::CheckoutCommit, window, cx| {
                crate::branch_dialogs::checkout_detached(
                    this,
                    action.repo,
                    action.oid.clone(),
                    window,
                    cx,
                )
            }),
        )
        .on_action(cx.listener(|this, action: &git::NewTagAt, window, cx| {
            new_tag(this, action.repo, action.oid.clone(), window, cx)
        }))
        .on_action(cx.listener(|this, action: &git::CherryPick, window, cx| {
            apply(this, action.repo, action.oids.clone(), false, window, cx)
        }))
        .on_action(
            cx.listener(|this, action: &git::RevertCommits, window, cx| {
                apply(this, action.repo, action.oids.clone(), true, window, cx)
            }),
        )
        .on_action(
            cx.listener(|this, action: &git::ResetToCommit, window, cx| {
                open_reset(this, action.repo, action.oid.clone(), window, cx)
            }),
        )
        .on_action(cx.listener(|this, action: &git::UndoCommit, window, cx| {
            undo_commit(this, action.repo, action.oid.clone(), window, cx)
        }))
        .on_action(cx.listener(|_, action: &CopyRevisionNumber, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(action.oids.join("\n")))
        }))
        .on_action(cx.listener(|this, action: &OpenCommitFile, window, cx| {
            this.open_file(action.path.clone(), true, window, cx)
        }));
    skip_actions(root, cx)
}

// --- Helpers ---

fn short(oid: &str) -> String {
    oid.chars().take(8).collect()
}

/// A notification title that names the repository when the window has several.
fn titled(git: &GitStore, repo: usize, title: String) -> String {
    if git.repos().len() > 1 {
        format!("{title} · {}", git.repo_name(repo))
    } else {
        title
    }
}

fn notify(workspace: &Workspace, repo: usize, notification: Notification, cx: &mut App) {
    workspace.git().update(cx, |git, cx| {
        let title = titled(git, repo, notification.title.to_string());
        git.notify(
            Notification {
                title: title.into(),
                ..notification
            },
            cx,
        )
    });
}

fn notify_error(workspace: &Workspace, repo: usize, title: &str, error: &GitError, cx: &mut App) {
    workspace.git().update(cx, |git, cx| {
        let title = titled(git, repo, title.to_string());
        git.notify_error(&title, error, cx)
    });
}

/// The stash message a cherry-pick or revert leaves when it stopped with local changes put aside
/// (`flux_git::history`).
fn autostash_message(revert: bool) -> &'static str {
    if revert {
        "Flux: uncommitted changes before revert"
    } else {
        "Flux: uncommitted changes before cherry-pick"
    }
}

/// How a cherry-pick or a revert ended, and whether local changes wait in a stash.
struct Applied {
    outcome: Outcome,
    stashed: bool,
}

impl GitStore {
    /// Cherry-picks (oldest first) or reverts (newest first) commits; local changes in the way are
    /// stashed around it.
    fn apply_commits(
        &mut self,
        repo: usize,
        oids: Vec<String>,
        revert: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<Applied, GitError>> {
        let activity = if revert {
            tr("Reverting…")
        } else {
            tr("Cherry-picking…")
        };
        self.run(repo, activity, cx, move |repo| {
            let outcome = if revert {
                flux_git::revert(repo, &oids, true)?
            } else {
                flux_git::cherry_pick(repo, &oids, true)?
            };
            // Stopped on conflicts with local changes put aside: they wait in the top stash.
            let stashed = outcome == Outcome::Conflicts
                && flux_git::stashes(repo)?
                    .first()
                    .is_some_and(|stash| stash.message.contains(autostash_message(revert)));
            Ok(Applied { outcome, stashed })
        })
    }

    /// `git cherry-pick --skip` / `git revert --skip`: past a commit whose changes are already
    /// there.
    fn skip_applied(
        &mut self,
        repo: usize,
        revert: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), GitError>> {
        self.run(repo, tr("Skipping…"), cx, move |repo| {
            let command = if revert { "revert" } else { "cherry-pick" };
            repo.git()
                .no_editor()
                .args([command, "--skip"])
                .output()
                .map(drop)
        })
    }

    fn reset_to(
        &mut self,
        repo: usize,
        oid: String,
        mode: ResetMode,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), GitError>> {
        self.run(repo, tr("Resetting…"), cx, move |repo| {
            flux_git::reset(repo, &oid, mode)
        })
    }

    fn create_tag(
        &mut self,
        repo: usize,
        name: String,
        oid: String,
        force: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), GitError>> {
        self.run(repo, tr("Creating the tag…"), cx, move |repo| {
            flux_git::create_tag(repo, &name, &oid, None, force)
        })
    }
}

// --- Cherry-pick and revert ---

/// Cherry-picks (`revert == false`) or reverts the selected commits (`oids` newest first).
fn apply(
    workspace: &mut Workspace,
    repo: usize,
    oids: Vec<String>,
    revert: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if oids.is_empty() {
        return;
    }
    // Cherry-pick goes oldest first; revert undoes the newest first.
    let ordered: Vec<String> = if revert {
        oids.clone()
    } else {
        oids.iter().rev().cloned().collect()
    };
    let count = oids.len();
    let task = workspace
        .git()
        .update(cx, |git, cx| git.apply_commits(repo, ordered, revert, cx));
    cx.spawn_in(window, async move |this, cx| {
        let result = task.await;
        this.update_in(cx, |this, window, cx| {
            report_applied(this, repo, count, revert, result, window, cx)
        })
        .ok();
    })
    .detach();
}

fn report_applied(
    workspace: &mut Workspace,
    repo: usize,
    count: usize,
    revert: bool,
    result: Result<Applied, GitError>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let (abort, failed) = if revert {
        (tr("Abort Revert"), tr("Revert failed"))
    } else {
        (tr("Abort Cherry-Pick"), tr("Cherry-pick failed"))
    };
    let applied = match result {
        Ok(applied) => applied,
        Err(error) => {
            // A commit whose changes are already on the branch: git stops with nothing to commit.
            let in_progress = workspace.git().read(cx).operation(repo).state;
            let stopped = matches!(in_progress, RepoState::CherryPicking | RepoState::Reverting);
            if stopped && is_empty_commit(&error) {
                let title = if revert {
                    tr("Nothing to revert: the changes are not on the branch")
                } else {
                    tr("Nothing to cherry-pick: the changes are already on the branch")
                };
                let notification = Notification::warning(title)
                    .body(tr("Skip this commit, or abort the operation."))
                    .action(tr("Skip"), SkipApplied { repo, revert })
                    .action(abort, git::AbortRepoOperation { repo });
                return notify(workspace, repo, notification, cx);
            }
            if let Some(blocked) = flux_git::blocked_by(&error)
                && !blocked.untracked.is_empty()
            {
                let notification =
                    Notification::warning(tr("Untracked files would be overwritten"))
                        .body(blocked.untracked.join("\n"));
                return notify(workspace, repo, notification, cx);
            }
            return notify_error(workspace, repo, failed, &error, cx);
        }
    };
    if applied.outcome == Outcome::Conflicts {
        crate::conflicts_dialog::show_conflicts(workspace, window, cx);
        let title = if revert {
            tr("Revert stopped on conflicts")
        } else {
            tr("Cherry-pick stopped on conflicts")
        };
        let mut body = tr("Resolve the conflicts, then continue.").to_string();
        if applied.stashed {
            body.push(' ');
            body.push_str(tr(
                "Your local changes are in the stash: unstash them once the operation is done.",
            ));
        }
        let notification = Notification::warning(title)
            .body(body)
            .action(tr("Resolve…"), git::ResolveConflicts)
            .action(abort, git::AbortRepoOperation { repo });
        return notify(workspace, repo, notification, cx);
    }
    let title = if revert {
        trn(count, "Reverted {n} commit", "Reverted {n} commits")
    } else {
        trn(
            count,
            "Cherry-picked {n} commit",
            "Cherry-picked {n} commits",
        )
    };
    notify(workspace, repo, Notification::success(title), cx);
}

/// git's words for a cherry-pick or revert that has nothing to commit.
fn is_empty_commit(error: &GitError) -> bool {
    error.details().is_some_and(|text| {
        text.contains("is now empty")
            || text.contains("nothing to commit")
            || text.contains("allow-empty")
    })
}

/// Skip of the commit a cherry-pick or a revert stopped on (the notification above).
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct SkipApplied {
    pub repo: usize,
    pub revert: bool,
}

fn skip_actions(root: Div, cx: &mut Context<Workspace>) -> Div {
    root.on_action(cx.listener(|this, action: &SkipApplied, window, cx| {
        let (repo, revert) = (action.repo, action.revert);
        let task = this
            .git()
            .update(cx, |git, cx| git.skip_applied(repo, revert, cx));
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                let result = result.map(|()| Applied {
                    outcome: match this.git().read(cx).operation(repo).state {
                        RepoState::Normal => Outcome::Done,
                        _ => Outcome::Conflicts,
                    },
                    stashed: false,
                });
                match result {
                    Ok(applied) if applied.outcome == Outcome::Done => {
                        notify(this, repo, Notification::success(tr("Commit skipped")), cx)
                    }
                    other => report_applied(this, repo, 1, revert, other, window, cx),
                }
            })
            .ok();
        })
        .detach();
    }))
}

// --- New Tag ---

fn new_tag(
    workspace: &mut Workspace,
    repo: usize,
    oid: String,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let git = workspace.git().clone();
    if git.read(cx).repos().get(repo).is_none() {
        return;
    }
    let weak: WeakEntity<Workspace> = cx.weak_entity();
    let validate_git = git.clone();
    workspace.toggle_modal(window, cx, move |window, cx| {
        InputDialog::new(tr("New Tag"), tr("Tag name"), window, cx)
            .subtitle(trf("at {0}", &[&short(&oid)]))
            .checkbox(tr("Overwrite existing tag"), false)
            .confirm_label(tr("Create"))
            .validate(
                move |name, checks, cx| {
                    if name.is_empty() {
                        return Err(tr("Enter a tag name").into());
                    }
                    flux_git::check_branch_name(name)
                        .map_err(|reason| -> gpui::SharedString { tr(reason).into() })?;
                    let exists = validate_git
                        .read(cx)
                        .refs(repo)
                        .tags
                        .iter()
                        .any(|tag| tag.name == name);
                    if exists && !checks.first().copied().unwrap_or(false) {
                        return Err(trf("Tag '{0}' already exists", &[&name]).into());
                    }
                    Ok(())
                },
                cx,
            )
            .on_confirm(move |name, checks, window, cx| {
                let force = checks.first().copied().unwrap_or(false);
                weak.update(cx, |workspace, cx| {
                    let task = workspace.git().update(cx, |git, cx| {
                        git.create_tag(repo, name.clone(), oid.clone(), force, cx)
                    });
                    cx.spawn_in(window, async move |this, cx| {
                        let result = task.await;
                        this.update(cx, |this, cx| match result {
                            Ok(()) => {
                                let title = trf("Created tag '{0}' at {1}", &[&name, &short(&oid)]);
                                notify(this, repo, Notification::success(title), cx)
                            }
                            Err(error) => {
                                notify_error(this, repo, tr("Tag not created"), &error, cx)
                            }
                        })
                        .ok();
                    })
                    .detach();
                })
                .ok();
            })
    });
}

// --- Undo Commit ---

/// Undo Commit: HEAD goes back to its parent, the changes stay staged (`reset --soft`); a commit
/// already pushed is asked about (undoing it needs a force push later).
fn undo_commit(
    workspace: &mut Workspace,
    repo: usize,
    oid: String,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let read = workspace.git().update(cx, |git, cx| {
        let oid = oid.clone();
        git.read(repo, cx, move |repo| {
            let pushed = flux_git::log::is_pushed(repo, &oid)?;
            let details = flux_git::log::commit_details(repo, &oid)?;
            Ok((pushed, details))
        })
    });
    cx.spawn_in(window, async move |this, cx| {
        let (pushed, details) = match read.await {
            Ok(read) => read,
            Err(error) => {
                this.update(cx, |this, cx| {
                    notify_error(this, repo, tr("Undo Commit failed"), &error, cx)
                })
                .ok();
                return;
            }
        };
        if pushed {
            let answer = this.update_in(cx, |_, window, cx| {
                window.prompt(
                    PromptLevel::Warning,
                    tr("The commit is already pushed"),
                    Some(tr(
                        "Undoing it rewrites history others may have: the branch will need a force push.",
                    )),
                    &[
                        PromptButton::new(tr("Undo Commit")),
                        PromptButton::cancel(tr("Cancel")),
                    ],
                    cx,
                )
            });
            let Ok(answer) = answer else {
                return;
            };
            if answer.await != Ok(0) {
                return;
            }
        }
        let Some(parent) = details.commit.parents.first().cloned() else {
            return;
        };
        let task = this.update(cx, |this, cx| {
            this.git()
                .update(cx, |git, cx| git.reset_to(repo, parent, ResetMode::Soft, cx))
        });
        let Ok(task) = task else {
            return;
        };
        let result = task.await;
        this.update_in(cx, |this, window, cx| match result {
            Ok(()) => {
                // As in JetBrains: the undone commit's message waits in the commit window.
                let panel = this.commit_panel(window, cx);
                panel.update(cx, |panel, cx| panel.set_message(&details.message, cx));
                let notification = Notification::success(tr("Commit undone"))
                    .body(trf(
                        "Its changes are back in the commit window: {0}",
                        &[&details.commit.summary],
                    ))
                    .action(tr("Commit…"), git::Commit);
                notify(this, repo, notification, cx)
            }
            Err(error) => notify_error(this, repo, tr("Undo Commit failed"), &error, cx),
        })
        .ok();
    })
    .detach();
}

// --- Reset ---

const RESET_MODES: [ResetMode; 4] = [
    ResetMode::Soft,
    ResetMode::Mixed,
    ResetMode::Hard,
    ResetMode::Keep,
];

fn mode_label(mode: ResetMode) -> &'static str {
    match mode {
        ResetMode::Soft => tr("Soft"),
        ResetMode::Mixed => tr("Mixed"),
        ResetMode::Hard => tr("Hard"),
        ResetMode::Keep => tr("Keep"),
    }
}

fn mode_note(mode: ResetMode) -> &'static str {
    match mode {
        ResetMode::Soft => {
            tr("Files aren't touched; the changes of the dropped commits stay staged.")
        }
        ResetMode::Mixed => {
            tr("Files aren't touched; the changes of the dropped commits stay, unstaged.")
        }
        ResetMode::Hard => tr(
            "Files become as in the commit: local changes and the dropped commits' changes are lost.",
        ),
        ResetMode::Keep => tr(
            "Files change as in the commit; local changes are kept, or the reset refuses if they are in the way.",
        ),
    }
}

fn open_reset(
    workspace: &mut Workspace,
    repo: usize,
    oid: String,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let git = workspace.git().read(cx);
    if git.repos().get(repo).is_none() {
        return;
    }
    let branch = git
        .current_branch(repo)
        .unwrap_or_else(|| tr("HEAD").to_string());
    let weak = cx.weak_entity();
    workspace.toggle_modal(window, cx, move |_, cx| ResetDialog {
        workspace: weak,
        repo,
        oid,
        branch,
        mode: ResetMode::Mixed,
        focus_handle: cx.focus_handle(),
    });
}

/// JetBrains' "Git Reset": where the branch goes and how the files follow.
pub struct ResetDialog {
    workspace: WeakEntity<Workspace>,
    repo: usize,
    oid: String,
    branch: String,
    mode: ResetMode,
    focus_handle: FocusHandle,
}

impl EventEmitter<DismissEvent> for ResetDialog {}

impl Focusable for ResetDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ResetDialog {
    fn step(&mut self, step: isize, cx: &mut Context<Self>) {
        let at = RESET_MODES
            .iter()
            .position(|m| *m == self.mode)
            .unwrap_or(1) as isize;
        let at = (at + step).rem_euclid(RESET_MODES.len() as isize) as usize;
        self.mode = RESET_MODES[at];
        cx.notify();
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (repo, oid, mode, branch) =
            (self.repo, self.oid.clone(), self.mode, self.branch.clone());
        cx.emit(DismissEvent);
        self.workspace
            .update(cx, |workspace, cx| {
                let task = workspace
                    .git()
                    .update(cx, |git, cx| git.reset_to(repo, oid.clone(), mode, cx));
                cx.spawn_in(window, async move |this, cx| {
                    let result = task.await;
                    this.update(cx, |this, cx| match result {
                        Ok(()) => {
                            let title = trf("Reset '{0}' to {1}", &[&branch, &short(&oid)]);
                            notify(this, repo, Notification::success(title), cx)
                        }
                        Err(error) => notify_error(this, repo, tr("Reset failed"), &error, cx),
                    })
                    .ok();
                })
                .detach();
            })
            .ok();
    }
}

impl Render for ResetDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        if !self.focus_handle.contains_focused(window, cx) {
            window.focus(&self.focus_handle);
        }
        let options = RESET_MODES.iter().enumerate().map(|(at, mode)| {
            let mode = *mode;
            let selected = self.mode == mode;
            div()
                .id(("reset-mode", at))
                .flex()
                .gap_2()
                .p_2()
                .rounded(px(ui::RADIUS_SM))
                .cursor_pointer()
                .hover(move |style| style.bg(ui.hover))
                .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    this.mode = mode;
                    cx.notify();
                    if event.click_count() == 2 {
                        this.confirm(window, cx);
                    }
                }))
                .child(
                    div()
                        .pt(px(2.))
                        .child(ui::radio(("reset-radio", at), selected, ui)),
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
                                .flex()
                                .items_center()
                                .gap_1()
                                .font_weight(FontWeight::MEDIUM)
                                .child(mode_label(mode))
                                .when(mode == ResetMode::Hard, |label| {
                                    label.child(
                                        crate::icons::icon(
                                            crate::icons::IconName::Warning,
                                            ui.warning,
                                        )
                                        .size(px(13.)),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .text_size(px(theme::TEXT_SM))
                                .text_color(if mode == ResetMode::Hard {
                                    ui.warning
                                } else {
                                    ui.dim
                                })
                                .child(mode_note(mode)),
                        ),
                )
        });
        ui::popover(ui)
            .key_context("ResetDialog")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &Confirm, window, cx| this.confirm(window, cx)))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(DismissEvent)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.step(1, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.step(-1, cx)))
            .w(px(RESET_WIDTH))
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_size(px(theme::TEXT_LG))
                            .child(tr("Git Reset")),
                    )
                    .child(div().text_color(ui.text_muted).child(trf(
                        "Reset '{0}' to {1}",
                        &[&self.branch, &short(&self.oid)],
                    ))),
            )
            .child(div().flex().flex_col().children(options))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        ui::text_button("reset-cancel", tr("Cancel"), false, ui)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    )
                    .child(
                        ui::primary_button("reset-confirm", tr("Reset"), true, ui)
                            .on_click(cx.listener(|this, _, window, cx| this.confirm(window, cx))),
                    ),
            )
    }
}
