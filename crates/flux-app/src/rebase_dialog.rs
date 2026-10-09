//! Rewriting history from the log (part D of stage 6.3): the interactive rebase dialog, as
//! JetBrains' "Rebasing Commits" (the plan from a commit up to HEAD: pick, edit, reword, squash,
//! fixup, drop, the order), and the log's shortcuts built on it — Edit Commit Message…, Fixup…,
//! Squash Into…, Drop Commits.
//!
//! The dialog lists the commits newest first, as the log does (the crate's plan is oldest first).
//! Keys in the list: ↑↓ (⇧ — extend the selection), P / E / R / S / F / D (⌫) — the action of the
//! selected commits, ⌥⇧↑ / ⌥⇧↓ — move them (or drag a row), ⇥ — the message field; ⌘↵ starts,
//! Esc cancels (in the message field — back to the list). Under the list: the selected commit's message — editable for a reword, and for a
//! squash the message of the commit the run becomes (prefilled with the messages joined).
//!
//! Rewriting commits that are already on a remote asks first (it will need a force push). A rebase
//! that stops — on conflicts (the Conflicts dialog, as after a merge) or at an `edit` (a
//! notification with Continue Rebase) — goes on through the operation flows of part 6.2.

use std::collections::HashMap;

use flux_core::Rope;
use flux_git::{GitError, Operation, Outcome, RebaseAction, RebaseEntry, RepoState};
use gpui::{
    App, ClickEvent, Context, DismissEvent, Div, DragMoveEvent, Entity, EventEmitter, FocusHandle,
    Focusable, FontWeight, KeyBinding, Render, ScrollHandle, SharedString, Subscription,
    WeakEntity, Window, actions, div, prelude::*, px,
};

use crate::dialog::Dialog;
use crate::editor::{Editor, EditorEvent};
use crate::git;
use crate::i18n::{tr, trf, trn};
use crate::notifications::Notification;
use crate::push_dialog::{age, now_seconds};
use crate::rename::difference;
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, RADIUS_MD, RADIUS_SM};
use crate::workspace::Workspace;

actions!(
    rebase_dialog,
    [
        SelectNext,
        SelectPrevious,
        ExtendNext,
        ExtendPrevious,
        Pick,
        Edit,
        Reword,
        Squash,
        Fixup,
        Drop,
        MoveUp,
        MoveDown,
        FocusMessage,
        Start,
        Dismiss,
    ]
);

// The message dialog (Edit Commit Message…, Squash Into…).
actions!(commit_message_dialog, [ConfirmMessage, DismissMessage]);

const WIDTH: f32 = 780.;
const LIST_HEIGHT: f32 = 300.;
const MESSAGE_HEIGHT: f32 = 120.;
const ROW_HEIGHT: f32 = 28.;
const ACTION_WIDTH: f32 = 64.;
const MESSAGE_DIALOG_WIDTH: f32 = 560.;
const MESSAGE_DIALOG_HEIGHT: f32 = 180.;
/// The commits listed in the Drop question.
const LISTED_COMMITS: usize = 5;

pub fn init(cx: &mut App) {
    let list = Some("RebaseList");
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, list),
        KeyBinding::new("up", SelectPrevious, list),
        KeyBinding::new("shift-down", ExtendNext, list),
        KeyBinding::new("shift-up", ExtendPrevious, list),
        KeyBinding::new("p", Pick, list),
        KeyBinding::new("e", Edit, list),
        KeyBinding::new("r", Reword, list),
        KeyBinding::new("s", Squash, list),
        KeyBinding::new("f", Fixup, list),
        KeyBinding::new("d", Drop, list),
        KeyBinding::new("backspace", Drop, list),
        KeyBinding::new("delete", Drop, list),
        KeyBinding::new("alt-shift-up", MoveUp, list),
        KeyBinding::new("alt-shift-down", MoveDown, list),
        KeyBinding::new("tab", FocusMessage, list),
    ]);
    let dialog = Some("RebaseDialog");
    cx.bind_keys([
        KeyBinding::new("cmd-enter", Start, dialog),
        KeyBinding::new("escape", Dismiss, dialog),
    ]);
    let message = Some("CommitMessageDialog");
    cx.bind_keys([
        KeyBinding::new("cmd-enter", ConfirmMessage, message),
        KeyBinding::new("escape", DismissMessage, message),
    ]);
}

/// The window-level actions: `InteractiveRebase`, `EditCommitMessage`, `MeldCommits`,
/// `DropCommits`.
pub fn workspace_actions(root: Div, cx: &mut Context<Workspace>) -> Div {
    root.on_action(
        cx.listener(|this, action: &git::InteractiveRebase, window, cx| {
            open_interactive(this, action.repo, action.oid.clone(), window, cx)
        }),
    )
    .on_action(
        cx.listener(|this, action: &git::EditCommitMessage, window, cx| {
            edit_message(this, action.repo, action.oid.clone(), window, cx)
        }),
    )
    .on_action(cx.listener(|this, action: &git::MeldCommits, window, cx| {
        meld(this, action.clone(), window, cx)
    }))
    .on_action(cx.listener(|this, action: &git::DropCommits, window, cx| {
        drop_commits(this, action.repo, action.oids.clone(), window, cx)
    }))
}

// --- The plan: rows of the dialog (newest first) ---

/// A commit of the dialog, as the plan has it, with what the list shows.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    oid: String,
    summary: String,
    action: RebaseAction,
    /// Reword: the new message; squash: the message of the commit its run becomes (held by the
    /// newest squash of the run).
    message: Option<String>,
    /// The user changed `message` (a squash's joined message isn't redone after that).
    edited: bool,
    /// The commit's whole message, author, time.
    full: String,
    author: String,
    time: i64,
}

/// The plan for the crate: oldest first.
fn plan_of(rows: &[Row]) -> Vec<RebaseEntry> {
    rows.iter()
        .rev()
        .map(|row| RebaseEntry {
            oid: row.oid.clone(),
            summary: row.summary.clone(),
            action: row.action,
            message: match row.action {
                RebaseAction::Reword | RebaseAction::Squash => row.message.clone(),
                _ => None,
            },
        })
        .collect()
}

fn melds(action: RebaseAction) -> bool {
    matches!(action, RebaseAction::Squash | RebaseAction::Fixup)
}

/// Why the plan can't run, if it can't.
fn problem(rows: &[Row]) -> Option<&'static str> {
    // The oldest commit that stays can't meld into anything.
    let oldest = rows
        .iter()
        .rev()
        .find(|row| row.action != RebaseAction::Drop);
    match oldest {
        None if !rows.is_empty() => Some("Every commit is dropped"),
        Some(row) if melds(row.action) => Some("The oldest commit has nothing to meld into"),
        _ => None,
    }
    .or_else(|| {
        rows.iter()
            .find(|row| {
                row.action == RebaseAction::Reword
                    && row.message.as_deref().is_none_or(|m| m.trim().is_empty())
            })
            .map(|_| "A reworded commit needs a message")
    })
}

/// The run a melding row belongs to (indices in the list): the commit it melds into (the nearest
/// older one that isn't dropped or melded) and the rows above it up to the newest melding one.
/// `None` — the row doesn't meld, or there is nothing to meld into.
fn run_of(rows: &[Row], index: usize) -> Option<(usize, Vec<usize>)> {
    if !melds(rows.get(index)?.action) {
        return None;
    }
    let target = (index + 1..rows.len())
        .find(|&at| !melds(rows[at].action) && rows[at].action != RebaseAction::Drop)?;
    let mut members = Vec::new();
    let mut at = target;
    while at > 0
        && matches!(
            rows[at - 1].action,
            RebaseAction::Squash | RebaseAction::Fixup | RebaseAction::Drop
        )
    {
        at -= 1;
        if melds(rows[at].action) {
            members.push(at);
        }
    }
    Some((target, members))
}

/// The messages of a run joined, oldest first, as git joins them for a squash (a fixup's is left
/// out).
fn joined_message(rows: &[Row], target: usize, members: &[usize]) -> String {
    let base = &rows[target];
    let mut parts = vec![match (base.action, &base.message) {
        (RebaseAction::Reword, Some(message)) => message.clone(),
        _ => base.full.clone(),
    }];
    // Members are newest last in `members` order reversed: walk from the target up.
    let mut squashed: Vec<usize> = members
        .iter()
        .copied()
        .filter(|&at| rows[at].action == RebaseAction::Squash)
        .collect();
    squashed.sort_unstable_by(|a, b| b.cmp(a));
    parts.extend(squashed.into_iter().map(|at| rows[at].full.clone()));
    parts
        .iter()
        .map(|part| part.trim_end())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// After the actions change: each run with a squash keeps its message on its newest squash (a
/// joined one, unless the user wrote their own); other rows lose a squash message.
fn settle_messages(rows: &mut [Row]) {
    let mut holders: HashMap<usize, (String, bool)> = HashMap::new();
    let mut seen = vec![false; rows.len()];
    for index in 0..rows.len() {
        if seen[index] {
            continue;
        }
        let Some((target, members)) = run_of(rows, index) else {
            continue;
        };
        for &at in &members {
            seen[at] = true;
        }
        let Some(&holder) = members
            .iter()
            .filter(|&&at| rows[at].action == RebaseAction::Squash)
            .min()
        else {
            continue;
        };
        // A message the user wrote stays with the run.
        let written = members
            .iter()
            .find(|&&at| rows[at].action == RebaseAction::Squash && rows[at].edited)
            .and_then(|&at| rows[at].message.clone());
        let message = match written {
            Some(message) => (message, true),
            None => (joined_message(rows, target, &members), false),
        };
        holders.insert(holder, message);
    }
    for (index, row) in rows.iter_mut().enumerate() {
        match holders.remove(&index) {
            Some((message, edited)) => {
                row.message = Some(message);
                row.edited = edited;
            }
            None if row.action != RebaseAction::Reword => {
                row.message = None;
                row.edited = false;
            }
            None => {}
        }
    }
}

/// Sets the action of the selected rows. A reword starts with the commit's message.
fn set_action(rows: &mut [Row], selected: &[usize], action: RebaseAction) {
    for &at in selected {
        let Some(row) = rows.get_mut(at) else {
            continue;
        };
        if row.action == action {
            continue;
        }
        row.action = action;
        row.edited = false;
        row.message = (action == RebaseAction::Reword).then(|| row.full.clone());
    }
    settle_messages(rows);
}

/// Moves the selected rows one step up (`up`) or down, keeping their order; returns the new
/// selection, or `None` when they are at the edge.
fn move_rows(rows: &mut [Row], selected: &[usize], up: bool) -> Option<Vec<usize>> {
    let mut selected = selected.to_vec();
    selected.sort_unstable();
    selected.dedup();
    let first = *selected.first()?;
    let last = *selected.last()?;
    if (up && first == 0) || (!up && last + 1 >= rows.len()) {
        return None;
    }
    if up {
        for &at in &selected {
            rows.swap(at - 1, at);
        }
    } else {
        for &at in selected.iter().rev() {
            rows.swap(at, at + 1);
        }
    }
    let moved = selected
        .into_iter()
        .map(|at| if up { at - 1 } else { at + 1 })
        .collect();
    settle_messages(rows);
    Some(moved)
}

/// Moves a row to a place in the list (an index between rows, as before the move); returns its
/// new index.
fn move_row_to(rows: &mut Vec<Row>, from: usize, to: usize) -> usize {
    let row = rows.remove(from);
    let to = if to > from { to - 1 } else { to }.min(rows.len());
    rows.insert(to, row);
    settle_messages(rows);
    to
}

/// The plan of Fixup… / Squash Into…: `oids` move right after `target` (oldest first, as they
/// were) and meld into it; a squash gets `message`. `plan` is oldest first.
fn meld_plan(
    plan: &[RebaseEntry],
    target: &str,
    oids: &[String],
    squash: bool,
    message: Option<String>,
) -> Option<Vec<RebaseEntry>> {
    let melded: Vec<RebaseEntry> = plan
        .iter()
        .filter(|entry| oids.contains(&entry.oid) && entry.oid != target)
        .cloned()
        .map(|mut entry| {
            entry.action = if squash {
                RebaseAction::Squash
            } else {
                RebaseAction::Fixup
            };
            entry.message = None;
            entry
        })
        .collect();
    if melded.is_empty() || melded.len() != oids.iter().filter(|oid| *oid != target).count() {
        return None;
    }
    let mut rest: Vec<RebaseEntry> = plan
        .iter()
        .filter(|entry| !oids.contains(&entry.oid) || entry.oid == target)
        .cloned()
        .collect();
    let at = rest.iter().position(|entry| entry.oid == target)?;
    let count = melded.len();
    rest.splice(at + 1..at + 1, melded);
    if squash && let Some(last) = rest.get_mut(at + count) {
        last.message = message;
    }
    Some(rest)
}

/// The plan of Drop Commits: `oids` dropped, the rest picked.
fn drop_plan(plan: &[RebaseEntry], oids: &[String]) -> Vec<RebaseEntry> {
    plan.iter()
        .cloned()
        .map(|mut entry| {
            if oids.contains(&entry.oid) {
                entry.action = RebaseAction::Drop;
            }
            entry
        })
        .collect()
}

fn short(oid: &str) -> &str {
    &oid[..oid.len().min(8)]
}

// --- Reading and running ---

/// A plan read for the dialog: its rows (newest first) and the commit it goes onto (`None` — from
/// the root commit).
struct Loaded {
    rows: Vec<Row>,
    onto: Option<String>,
}

/// Reads the plan from `oid` up to HEAD with every commit's message, author and date.
fn load_plan(repo: &flux_git::Repo, oid: &str) -> Result<Loaded, GitError> {
    let plan = flux_git::rebase::rebase_plan(repo, oid)?;
    let onto = flux_git::branch::resolve(repo, &format!("{oid}^"))?;
    let mut rows = Vec::with_capacity(plan.len());
    for entry in plan.into_iter().rev() {
        let details = flux_git::log::commit_details(repo, &entry.oid)?;
        rows.push(Row {
            oid: entry.oid,
            summary: entry.summary,
            action: RebaseAction::Pick,
            message: None,
            edited: false,
            full: details.message,
            author: details.commit.author,
            time: details.commit.author_time,
        });
    }
    Ok(Loaded { rows, onto })
}

/// What a rewrite is, for its notifications.
#[derive(Debug, Clone)]
enum Rewrite {
    Interactive { count: usize },
    Reword,
    Meld { squash: bool, count: usize },
    Drop { count: usize },
}

/// How a rewrite ended, read right after it (the store's status comes later).
struct Ended {
    outcome: Outcome,
    operation: Operation,
    conflicts: bool,
}

fn ended(repo: &flux_git::Repo, outcome: Outcome) -> Ended {
    let conflicts = repo
        .git()
        .read_only()
        .args(["diff", "--name-only", "--diff-filter=U"])
        .output_string()
        .is_ok_and(|files| !files.trim().is_empty());
    Ended {
        outcome,
        operation: flux_git::operation(repo),
        conflicts,
    }
}

/// Refuses while the repository is in the middle of an operation.
fn busy(workspace: &mut Workspace, repo: usize, cx: &mut Context<Workspace>) -> bool {
    let state = workspace.git().read(cx).operation(repo).state;
    if matches!(state, RepoState::Normal | RepoState::Bisecting) {
        return false;
    }
    workspace.show_message(tr("Finish the operation in progress first").into(), cx);
    true
}

fn notify(workspace: &Workspace, notification: Notification, cx: &mut App) {
    workspace
        .git()
        .update(cx, |git, cx| git.notify(notification, cx));
}

fn notify_error(workspace: &Workspace, title: &str, error: &GitError, cx: &mut App) {
    workspace
        .git()
        .update(cx, |git, cx| git.notify_error(title, error, cx));
}

/// Asks first if `oldest` (the oldest commit rewritten) is on a remote; then runs `job`.
fn rewrite(
    workspace: &mut Workspace,
    repo: usize,
    oldest: String,
    kind: Rewrite,
    job: impl FnOnce(&flux_git::Repo) -> Result<Outcome, GitError> + Send + 'static,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let git = workspace.git().clone();
    let pushed = git.update(cx, |git, cx| {
        git.read(repo, cx, move |repo| {
            flux_git::log::is_pushed(repo, &oldest)
        })
    });
    cx.spawn_in(window, async move |this, cx| {
        // Can't tell — go on: the push will say if it is rejected.
        let pushed = pushed.await.unwrap_or(false);
        if pushed {
            let Ok(answer) = this.update_in(cx, |_, window, cx| {
                Dialog::warning(tr("These commits are already pushed"))
                    .message(tr(
                        "Rewriting them changes the history others may have. The branch will need a force push.",
                    ))
                    .danger(tr("Rewrite Anyway"))
                    .cancel(tr("Cancel"))
                    .show(window, cx)
            }) else {
                return;
            };
            if answer.await != Some(0) {
                return;
            }
        }
        let Ok(task) = this.update(cx, |this, cx| {
            this.git().update(cx, |git, cx| {
                git.run(repo, tr("Rebasing…"), cx, move |repo| {
                    job(repo).map(|outcome| ended(repo, outcome))
                })
            })
        }) else {
            return;
        };
        let result = task.await;
        this.update_in(cx, |this, window, cx| report(this, repo, kind, result, window, cx))
            .ok();
    })
    .detach();
}

/// Tells how a rewrite went: done, stopped on conflicts, or stopped at an `edit`.
fn report(
    workspace: &mut Workspace,
    repo: usize,
    kind: Rewrite,
    result: Result<Ended, GitError>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let ended = match result {
        Ok(ended) => ended,
        Err(error) => {
            let title = match kind {
                Rewrite::Reword => tr("Couldn't edit the commit message"),
                _ => tr("Rebase failed"),
            };
            return notify_error(workspace, title, &error, cx);
        }
    };
    if ended.outcome == Outcome::Conflicts {
        let rebasing = ended.operation.state == RepoState::Rebasing;
        if ended.conflicts {
            crate::conflicts_dialog::show_conflicts(workspace, window, cx);
            let mut notification = Notification::warning(if rebasing {
                tr("Rebase stopped on conflicts")
            } else {
                tr("Local changes came back with conflicts")
            });
            notification = notification.body(if rebasing {
                tr("Resolve the conflicts, then continue the rebase.")
            } else {
                tr("Resolve the conflicts of the changes stashed around the rebase.")
            });
            notification = notification.action(tr("Resolve…"), git::ResolveConflicts);
            if rebasing {
                notification =
                    notification.action(tr("Abort Rebase"), git::AbortRepoOperation { repo });
            }
            return notify(workspace, notification, cx);
        }
        if rebasing {
            // Stopped at an `edit`.
            let at = ended
                .operation
                .stopped_at
                .as_deref()
                .map(short)
                .unwrap_or_default()
                .to_string();
            let notification = Notification::info(trf("Stopped at {0} for editing", &[&at]))
                .body(tr(
                    "Amend the commit as you need, then continue the rebase.",
                ))
                .action(tr("Continue Rebase"), git::ContinueRepoOperation { repo })
                .action(tr("Abort Rebase"), git::AbortRepoOperation { repo });
            return notify(workspace, notification, cx);
        }
    }
    let title = match kind {
        Rewrite::Interactive { count } => trn(count, "Rebased {n} commit", "Rebased {n} commits"),
        Rewrite::Reword => tr("Commit message updated").to_string(),
        Rewrite::Meld {
            squash: true,
            count,
        } => trn(count, "Squashed {n} commit", "Squashed {n} commits"),
        Rewrite::Meld {
            squash: false,
            count,
        } => trn(count, "Fixed up {n} commit", "Fixed up {n} commits"),
        Rewrite::Drop { count } => trn(count, "Dropped {n} commit", "Dropped {n} commits"),
    };
    notify(workspace, Notification::success(title), cx);
}

// --- The flows ---

fn open_interactive(
    workspace: &mut Workspace,
    repo: usize,
    oid: String,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if busy(workspace, repo, cx) {
        return;
    }
    let git = workspace.git().clone();
    let branch = git
        .read(cx)
        .current_branch(repo)
        .unwrap_or_else(|| "HEAD".into());
    let read = git.update(cx, |git, cx| {
        let oid = oid.clone();
        git.read(repo, cx, move |repo| load_plan(repo, &oid))
    });
    cx.spawn_in(window, async move |this, cx| {
        let loaded = read.await;
        this.update_in(cx, |this, window, cx| match loaded {
            Err(error) => notify_error(this, tr("Can't rebase from this commit"), &error, cx),
            Ok(loaded) => {
                let workspace = cx.entity().downgrade();
                this.toggle_dialog(window, cx, move |window, cx| {
                    RebaseDialog::new(workspace, repo, branch, loaded, window, cx)
                });
            }
        })
        .ok();
    })
    .detach();
}

fn edit_message(
    workspace: &mut Workspace,
    repo: usize,
    oid: String,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if busy(workspace, repo, cx) {
        return;
    }
    let read = workspace.git().update(cx, |git, cx| {
        let oid = oid.clone();
        git.read(repo, cx, move |repo| {
            flux_git::log::commit_details(repo, &oid)
        })
    });
    cx.spawn_in(window, async move |this, cx| {
        let details = read.await;
        this.update_in(cx, |this, window, cx| {
            let details = match details {
                Ok(details) => details,
                Err(error) => {
                    return notify_error(this, tr("Couldn't read the commit"), &error, cx);
                }
            };
            let subtitle = format!("{} · {}", short(&oid), details.commit.summary);
            let workspace = cx.entity().downgrade();
            this.toggle_dialog(window, cx, move |window, cx| {
                MessageDialog::new(
                    tr("Edit Commit Message"),
                    subtitle,
                    &details.message,
                    tr("OK"),
                    window,
                    cx,
                )
                .on_confirm(move |message, window, cx| {
                    workspace
                        .update(cx, |this, cx| {
                            let target = oid.clone();
                            rewrite(
                                this,
                                repo,
                                oid,
                                Rewrite::Reword,
                                move |repo| flux_git::rebase::reword(repo, &target, &message),
                                window,
                                cx,
                            )
                        })
                        .ok();
                })
            });
        })
        .ok();
    })
    .detach();
}

/// What Fixup… / Squash Into… needs: the plan from the oldest commit involved, the commit it goes
/// onto, and the messages of the commits that meld (for a squash).
struct MeldPlan {
    plan: Vec<RebaseEntry>,
    onto: Option<String>,
    messages: Vec<String>,
}

fn read_meld(repo: &flux_git::Repo, target: &str, oids: &[String]) -> Result<MeldPlan, GitError> {
    // The oldest of `oids` is the last one; the target may be older still.
    let oldest = oids.last().map(String::as_str).unwrap_or(target);
    let mut from = oldest.to_string();
    let mut plan = flux_git::rebase::rebase_plan(repo, oldest)?;
    if !plan.iter().any(|entry| entry.oid == target) {
        from = target.to_string();
        plan = flux_git::rebase::rebase_plan(repo, target)?;
    }
    let onto = flux_git::branch::resolve(repo, &format!("{from}^"))?;
    // The target's message first, then the others oldest first.
    let mut messages = Vec::new();
    for entry in &plan {
        if entry.oid == target || oids.contains(&entry.oid) {
            let message = flux_git::log::commit_details(repo, &entry.oid)?.message;
            if entry.oid == target {
                messages.insert(0, message);
            } else {
                messages.push(message);
            }
        }
    }
    Ok(MeldPlan {
        plan,
        onto,
        messages,
    })
}

fn meld(
    workspace: &mut Workspace,
    action: git::MeldCommits,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let repo = action.repo;
    if busy(workspace, repo, cx) {
        return;
    }
    let read = workspace.git().update(cx, |git, cx| {
        let (target, oids) = (action.target.clone(), action.oids.clone());
        git.read(repo, cx, move |repo| read_meld(repo, &target, &oids))
    });
    cx.spawn_in(window, async move |this, cx| {
        let read = read.await;
        this.update_in(cx, |this, window, cx| {
            let meld = match read {
                Ok(meld) => meld,
                Err(error) => {
                    return notify_error(this, tr("Can't meld these commits"), &error, cx);
                }
            };
            let count = action
                .oids
                .iter()
                .filter(|oid| **oid != action.target)
                .count();
            let kind = Rewrite::Meld {
                squash: action.squash,
                count,
            };
            let target = action.target.clone();
            let run = move |this: &mut Workspace,
                            message: Option<String>,
                            window: &mut Window,
                            cx: &mut Context<Workspace>| {
                let Some(plan) = meld_plan(
                    &meld.plan,
                    &action.target,
                    &action.oids,
                    action.squash,
                    message,
                ) else {
                    return this
                        .show_message(tr("These commits aren't on the current branch").into(), cx);
                };
                let oldest = plan
                    .first()
                    .map(|entry| entry.oid.clone())
                    .unwrap_or_default();
                let onto = meld.onto.clone();
                rewrite(
                    this,
                    repo,
                    oldest,
                    kind,
                    move |repo| {
                        flux_git::rebase::interactive_rebase(repo, onto.as_deref(), &plan, true)
                    },
                    window,
                    cx,
                )
            };
            if !action.squash {
                return run(this, None, window, cx);
            }
            let joined = meld
                .messages
                .iter()
                .map(|message| message.trim_end())
                .collect::<Vec<_>>()
                .join("\n\n");
            let subtitle = trf("Into {0}", &[&short(&target)]);
            let workspace = cx.entity().downgrade();
            this.toggle_dialog(window, cx, move |window, cx| {
                MessageDialog::new(
                    trn(count + 1, "Squash {n} Commit", "Squash {n} Commits"),
                    subtitle,
                    &joined,
                    tr("Squash"),
                    window,
                    cx,
                )
                .on_confirm(move |message, window, cx| {
                    workspace
                        .update(cx, |this, cx| run(this, Some(message), window, cx))
                        .ok();
                })
            });
        })
        .ok();
    })
    .detach();
}

fn drop_commits(
    workspace: &mut Workspace,
    repo: usize,
    oids: Vec<String>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if busy(workspace, repo, cx) || oids.is_empty() {
        return;
    }
    let read = workspace.git().update(cx, |git, cx| {
        let oldest = oids.last().cloned().unwrap_or_default();
        git.read(repo, cx, move |repo| {
            let plan = flux_git::rebase::rebase_plan(repo, &oldest)?;
            let onto = flux_git::branch::resolve(repo, &format!("{oldest}^"))?;
            Ok((plan, onto))
        })
    });
    cx.spawn_in(window, async move |this, cx| {
        let read = read.await;
        let Ok(answer) = this.update_in(cx, |this, window, cx| {
            let (plan, onto) = match read {
                Ok(read) => read,
                Err(error) => {
                    notify_error(this, tr("Can't drop these commits"), &error, cx);
                    return None;
                }
            };
            let listed: Vec<String> = plan
                .iter()
                .rev()
                .filter(|entry| oids.contains(&entry.oid))
                .take(LISTED_COMMITS)
                .map(|entry| format!("{} {}", short(&entry.oid), entry.summary))
                .collect();
            let mut detail = listed.join("\n");
            if oids.len() > LISTED_COMMITS {
                detail.push('\n');
                detail.push_str(&trf("and {0} more", &[&(oids.len() - LISTED_COMMITS)]));
            }
            let answer = Dialog::warning(trn(oids.len(), "Drop {n} commit?", "Drop {n} commits?"))
                .details(detail)
                .danger(tr("Drop"))
                .cancel(tr("Cancel"))
                .show(window, cx);
            Some((answer, plan, onto))
        }) else {
            return;
        };
        let Some((answer, plan, onto)) = answer else {
            return;
        };
        if answer.await != Some(0) {
            return;
        }
        this.update_in(cx, |this, window, cx| {
            let plan = drop_plan(&plan, &oids);
            let oldest = plan
                .first()
                .map(|entry| entry.oid.clone())
                .unwrap_or_default();
            rewrite(
                this,
                repo,
                oldest,
                Rewrite::Drop { count: oids.len() },
                move |repo| {
                    flux_git::rebase::interactive_rebase(repo, onto.as_deref(), &plan, true)
                },
                window,
                cx,
            )
        })
        .ok();
    })
    .detach();
}

// --- The interactive rebase dialog ---

/// A row being dragged to a new place.
#[derive(Clone)]
struct DraggedRow {
    index: usize,
    label: SharedString,
}

impl Render for DraggedRow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        ui::popover(ui)
            .px_2()
            .py_1()
            .text_size(px(theme::TEXT_SM))
            .child(self.label.clone())
    }
}

pub struct RebaseDialog {
    workspace: WeakEntity<Workspace>,
    repo: usize,
    branch: String,
    onto: Option<String>,
    rows: Vec<Row>,
    /// Selected rows; `cursor` is where ↑↓ go from, `anchor` where ⇧ extends from.
    selected: Vec<usize>,
    cursor: usize,
    anchor: usize,
    /// The message field: a reword's message or a squash run's.
    message: Entity<Editor>,
    /// The row whose message is in the field (by commit).
    editing: Option<String>,
    /// Where a dragged row would land (an index between rows).
    drop_at: Option<usize>,
    /// The dialog's own focus (the overlay layer watches it): it passes to the list.
    focus_handle: FocusHandle,
    list_focus: FocusHandle,
    scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DismissEvent> for RebaseDialog {}

impl RebaseDialog {
    fn new(
        workspace: WeakEntity<Workspace>,
        repo: usize,
        branch: String,
        loaded: Loaded,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let message = cx.new(|cx| Editor::message(tr("Commit Message"), window, cx));
        let subscriptions = vec![cx.subscribe(&message, |this, _, event: &EditorEvent, cx| {
            if matches!(event, EditorEvent::Edited) {
                this.store_message(cx);
            }
        })];
        let focus_handle = cx.focus_handle();
        let list_focus = cx.focus_handle();
        let mut subscriptions = subscriptions;
        subscriptions.push(cx.on_focus(&focus_handle, window, |this, window, _| {
            window.focus(&this.list_focus)
        }));
        let mut dialog = Self {
            workspace,
            repo,
            branch,
            onto: loaded.onto,
            rows: loaded.rows,
            selected: vec![0],
            cursor: 0,
            anchor: 0,
            message,
            editing: None,
            drop_at: None,
            focus_handle,
            list_focus,
            scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        };
        dialog.load_message(cx);
        dialog
    }

    /// The row whose message the field edits for the selection: a reword, or the holder of a
    /// squash run's message.
    fn message_row(&self) -> Option<usize> {
        let &[at] = self.selected.as_slice() else {
            return None;
        };
        let row = self.rows.get(at)?;
        match row.action {
            RebaseAction::Reword => Some(at),
            RebaseAction::Squash | RebaseAction::Fixup => {
                let (_, members) = run_of(&self.rows, at)?;
                members
                    .into_iter()
                    .filter(|&at| self.rows[at].action == RebaseAction::Squash)
                    .min()
            }
            _ => None,
        }
    }

    /// The field's text goes to its row.
    fn store_message(&mut self, cx: &mut Context<Self>) {
        let Some(oid) = &self.editing else {
            return;
        };
        let text = self.message.read(cx).document.text().to_string();
        if let Some(row) = self.rows.iter_mut().find(|row| &row.oid == oid)
            && row.message.as_deref() != Some(text.as_str())
        {
            row.message = Some(text);
            row.edited = true;
            cx.notify();
        }
    }

    /// The field shows the message of the selection's row, if it has one.
    fn load_message(&mut self, cx: &mut Context<Self>) {
        let row = self.message_row();
        let oid = row.map(|at| self.rows[at].oid.clone());
        let text = row
            .and_then(|at| self.rows[at].message.clone())
            .unwrap_or_default();
        self.editing = None;
        self.message.update(cx, |editor, cx| {
            let new = Rope::from_str(&text);
            if let Some((range, text)) = difference(editor.document.text(), &new) {
                editor.replace_ranges(vec![(range, text)], cx);
                editor
                    .document
                    .set_selection(flux_core::Selection::point(0));
                editor.scroll = gpui::point(0., 0.);
                editor.autoscroll = None;
                cx.notify();
            }
        });
        self.editing = oid;
        cx.notify();
    }

    fn select(&mut self, at: usize, extend: bool, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let at = at.min(self.rows.len() - 1);
        self.cursor = at;
        if extend {
            let (from, to) = (self.anchor.min(at), self.anchor.max(at));
            self.selected = (from..=to).collect();
        } else {
            self.anchor = at;
            self.selected = vec![at];
        }
        self.scroll.scroll_to_item(at);
        self.load_message(cx);
    }

    fn toggle_selected(&mut self, at: usize, cx: &mut Context<Self>) {
        match self.selected.iter().position(|&s| s == at) {
            Some(pos) if self.selected.len() > 1 => {
                self.selected.remove(pos);
            }
            Some(_) => {}
            None => {
                self.selected.push(at);
                self.selected.sort_unstable();
            }
        }
        self.cursor = at;
        self.anchor = at;
        self.load_message(cx);
    }

    fn step(&mut self, down: bool, extend: bool, cx: &mut Context<Self>) {
        let at = if down {
            (self.cursor + 1).min(self.rows.len().saturating_sub(1))
        } else {
            self.cursor.saturating_sub(1)
        };
        self.select(at, extend, cx);
    }

    fn set_action(&mut self, action: RebaseAction, cx: &mut Context<Self>) {
        set_action(&mut self.rows, &self.selected, action);
        self.load_message(cx);
    }

    fn move_selected(&mut self, up: bool, cx: &mut Context<Self>) {
        if let Some(moved) = move_rows(&mut self.rows, &self.selected, up) {
            let shift = |at: usize| if up { at - 1 } else { at + 1 };
            self.cursor = shift(self.cursor);
            self.anchor = shift(self.anchor);
            self.selected = moved;
            self.scroll.scroll_to_item(self.cursor);
            self.load_message(cx);
        }
    }

    fn drop_row(&mut self, from: usize, cx: &mut Context<Self>) {
        let Some(to) = self.drop_at.take() else {
            return;
        };
        let at = move_row_to(&mut self.rows, from, to);
        self.select(at, false, cx);
    }

    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if problem(&self.rows).is_some() {
            return;
        }
        let plan = plan_of(&self.rows);
        let count = self.rows.len();
        let Some(oldest) = plan.first().map(|entry| entry.oid.clone()) else {
            return;
        };
        let (repo, onto) = (self.repo, self.onto.clone());
        cx.emit(DismissEvent);
        self.workspace
            .update(cx, |workspace, cx| {
                rewrite(
                    workspace,
                    repo,
                    oldest,
                    Rewrite::Interactive { count },
                    move |repo| {
                        flux_git::rebase::interactive_rebase(repo, onto.as_deref(), &plan, true)
                    },
                    window,
                    cx,
                )
            })
            .ok();
    }

    // --- Rendering ---

    fn render_toolbar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let focus = self.list_focus.clone();
        let button = |id: &'static str,
                      label: &'static str,
                      action: RebaseAction,
                      key: Box<dyn gpui::Action>,
                      cx: &mut Context<Self>| {
            let keys = ui::shortcut_in(key.as_ref(), &focus, window);
            ui::text_button(id, tr(label), false, ui)
                .tooltip(ui::tooltip(tr(label), keys))
                .on_click(
                    cx.listener(move |this, _: &ClickEvent, _, cx| this.set_action(action, cx)),
                )
        };
        let mover = |id: &'static str,
                     icon: crate::icons::IconName,
                     label: &'static str,
                     up: bool,
                     key: Box<dyn gpui::Action>,
                     cx: &mut Context<Self>| {
            let keys = ui::shortcut_in(key.as_ref(), &focus, window);
            ui::icon_button(id, icon, ui)
                .tooltip(ui::tooltip(tr(label), keys))
                .on_click(
                    cx.listener(move |this, _: &ClickEvent, _, cx| this.move_selected(up, cx)),
                )
        };
        div()
            .flex()
            .items_center()
            .gap_1p5()
            .child(button(
                "rebase-pick",
                "Pick",
                RebaseAction::Pick,
                Box::new(Pick),
                cx,
            ))
            .child(button(
                "rebase-edit",
                "Edit",
                RebaseAction::Edit,
                Box::new(Edit),
                cx,
            ))
            .child(button(
                "rebase-reword",
                "Reword",
                RebaseAction::Reword,
                Box::new(Reword),
                cx,
            ))
            .child(button(
                "rebase-squash",
                "Squash",
                RebaseAction::Squash,
                Box::new(Squash),
                cx,
            ))
            .child(button(
                "rebase-fixup",
                "Fixup",
                RebaseAction::Fixup,
                Box::new(Fixup),
                cx,
            ))
            .child(button(
                "rebase-drop",
                "Drop",
                RebaseAction::Drop,
                Box::new(Drop),
                cx,
            ))
            .child(div().flex_1())
            .child(mover(
                "rebase-up",
                crate::icons::IconName::ArrowUp,
                "Move Up",
                true,
                Box::new(MoveUp),
                cx,
            ))
            .child(mover(
                "rebase-down",
                crate::icons::IconName::ArrowDown,
                "Move Down",
                false,
                Box::new(MoveDown),
                cx,
            ))
    }

    fn render_row(
        &self,
        index: usize,
        focused: bool,
        now: i64,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let row = &self.rows[index];
        let selected = self.selected.contains(&index);
        let dropped = row.action == RebaseAction::Drop;
        let (label, color) = action_badge(row.action, &ui);
        // A reword shows its new first line.
        let summary = match (row.action, &row.message) {
            (RebaseAction::Reword, Some(message)) => {
                message.lines().next().unwrap_or_default().to_string()
            }
            _ => row.summary.clone(),
        };
        let dragged = DraggedRow {
            index,
            label: summary.clone().into(),
        };
        let marker_above = self.drop_at == Some(index);
        let marker_below = self.drop_at == Some(index + 1) && index + 1 == self.rows.len();
        div()
            .px_1()
            .relative()
            .child(
                div()
                    .id(("rebase-row", index))
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
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        window.focus(&this.list_focus);
                        let modifiers = event.modifiers();
                        if modifiers.platform {
                            this.toggle_selected(index, cx);
                        } else {
                            this.select(index, modifiers.shift, cx);
                        }
                    }))
                    .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
                    .on_drag_move(cx.listener(
                        move |this, event: &DragMoveEvent<DraggedRow>, _, cx| {
                            let bounds = event.bounds;
                            let y = event.event.position.y;
                            if !bounds.contains(&event.event.position) {
                                return;
                            }
                            let at = if y < bounds.center().y {
                                index
                            } else {
                                index + 1
                            };
                            if this.drop_at != Some(at) {
                                this.drop_at = Some(at);
                                cx.notify();
                            }
                        },
                    ))
                    .on_drop(cx.listener(|this, dragged: &DraggedRow, _, cx| {
                        this.drop_row(dragged.index, cx)
                    }))
                    .child(
                        div()
                            .flex_none()
                            .w(px(ACTION_WIDTH))
                            .child(ui::badge(label, color)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_family(theme::code_font())
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.dim)
                            .child(short(&row.oid).to_string()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .when(dropped, |text| {
                                text.line_through().text_color(ui.text_disabled)
                            })
                            .when(melds(row.action), |text| text.text_color(ui.text_muted))
                            .child(summary),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.dim)
                            .child(format!("{}, {}", row.author, age(now, row.time))),
                    ),
            )
            .when(marker_above || marker_below, |row| {
                row.child(
                    div()
                        .absolute()
                        .left(px(8.))
                        .right(px(8.))
                        .h(px(2.))
                        .rounded(px(1.))
                        .bg(ui.accent)
                        .when(marker_above, |marker| marker.top(px(-1.)))
                        .when(marker_below, |marker| marker.bottom(px(-1.))),
                )
            })
    }

    fn render_message(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let editable = self.editing.is_some();
        let caption = match self.message_row().map(|at| self.rows[at].action) {
            Some(RebaseAction::Reword) => tr("New message"),
            Some(_) => tr("Message of the squashed commit"),
            None if self.selected.len() == 1 => tr("Message"),
            None => tr("Select one commit to see its message"),
        };
        let focused = self.message.focus_handle(cx).is_focused(window);
        let preview = (!editable && self.selected.len() == 1)
            .then(|| self.rows.get(self.cursor).map(|row| row.full.clone()))
            .flatten();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(ui::section_label(caption, ui))
            .child(
                div()
                    .id("rebase-message")
                    // Esc in the field (with nothing selected in it) goes back to the list.
                    .on_action(cx.listener(|this, _: &crate::editor::Cancel, window, _| {
                        window.focus(&this.list_focus)
                    }))
                    .h(px(MESSAGE_HEIGHT))
                    .rounded(px(RADIUS_MD))
                    .border_1()
                    .when(editable, |field| {
                        field
                            .pl_1()
                            .pt_1()
                            .bg(ui.input_background)
                            .border_color(if focused {
                                ui.focus_border
                            } else {
                                ui.input_border
                            })
                            .when(focused, |field| field.shadow(ui::focus_ring(ui)))
                            .child(self.message.clone())
                    })
                    .when(!editable, |field| {
                        field
                            .p_2()
                            .border_color(ui.divider)
                            .overflow_y_scroll()
                            .font_family(theme::code_font())
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.text_muted)
                            .children(preview)
                    }),
            )
    }
}

/// The label and color of an action in the list.
fn action_badge(action: RebaseAction, ui: &UiColors) -> (&'static str, gpui::Hsla) {
    match action {
        RebaseAction::Pick => (tr("pick"), ui.text_muted),
        RebaseAction::Edit => (tr("edit"), ui.blue),
        RebaseAction::Reword => (tr("reword"), ui.violet),
        RebaseAction::Squash => (tr("squash"), ui.amber),
        RebaseAction::Fixup => (tr("fixup"), ui.orange),
        RebaseAction::Drop => (tr("drop"), ui.error),
    }
}

// The dialog's focus contains the list's and the message field's: moving between them doesn't
// count as leaving the dialog.
impl Focusable for RebaseDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RebaseDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        if self.drop_at.is_some() && !cx.has_active_drag() {
            self.drop_at = None;
        }
        let focused = self.list_focus.is_focused(window);
        let now = now_seconds();
        let rows: Vec<_> = (0..self.rows.len())
            .map(|index| self.render_row(index, focused, now, cx))
            .collect();
        let problem = problem(&self.rows);
        let onto = match &self.onto {
            Some(onto) => short(onto).to_string(),
            None => tr("the root").to_string(),
        };
        ui::popover(ui)
            .key_context("RebaseDialog")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &Start, window, cx| this.start(window, cx)))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(DismissEvent)))
            .w(px(WIDTH))
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(trn(
                        self.rows.len(),
                        "Rebasing {n} commit",
                        "Rebasing {n} commits",
                    )))
                    .child(
                        div()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.dim)
                            .child(trf("{0} onto {1}", &[&self.branch, &onto])),
                    ),
            )
            .child(self.render_toolbar(window, cx))
            .child(
                div()
                    .id("rebase-list")
                    .key_context("RebaseList")
                    .track_focus(&self.list_focus)
                    .on_action(
                        cx.listener(|this, _: &SelectNext, _, cx| this.step(true, false, cx)),
                    )
                    .on_action(
                        cx.listener(|this, _: &SelectPrevious, _, cx| this.step(false, false, cx)),
                    )
                    .on_action(cx.listener(|this, _: &ExtendNext, _, cx| this.step(true, true, cx)))
                    .on_action(
                        cx.listener(|this, _: &ExtendPrevious, _, cx| this.step(false, true, cx)),
                    )
                    .on_action(
                        cx.listener(|this, _: &Pick, _, cx| {
                            this.set_action(RebaseAction::Pick, cx)
                        }),
                    )
                    .on_action(
                        cx.listener(|this, _: &Edit, _, cx| {
                            this.set_action(RebaseAction::Edit, cx)
                        }),
                    )
                    .on_action(cx.listener(|this, _: &Reword, window, cx| {
                        this.set_action(RebaseAction::Reword, cx);
                        if this.editing.is_some() {
                            window.focus(&this.message.focus_handle(cx));
                        }
                    }))
                    .on_action(cx.listener(|this, _: &Squash, _, cx| {
                        this.set_action(RebaseAction::Squash, cx)
                    }))
                    .on_action(cx.listener(|this, _: &Fixup, _, cx| {
                        this.set_action(RebaseAction::Fixup, cx)
                    }))
                    .on_action(
                        cx.listener(|this, _: &Drop, _, cx| {
                            this.set_action(RebaseAction::Drop, cx)
                        }),
                    )
                    .on_action(cx.listener(|this, _: &MoveUp, _, cx| this.move_selected(true, cx)))
                    .on_action(
                        cx.listener(|this, _: &MoveDown, _, cx| this.move_selected(false, cx)),
                    )
                    .on_action(cx.listener(|this, _: &FocusMessage, window, cx| {
                        if this.editing.is_some() {
                            window.focus(&this.message.focus_handle(cx));
                        }
                    }))
                    .h(px(LIST_HEIGHT))
                    .py_1()
                    .rounded(px(RADIUS_MD))
                    .border_1()
                    .border_color(ui.divider)
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(rows),
            )
            .child(self.render_message(window, cx))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(theme::TEXT_SM))
                            .map(|line| match problem {
                                Some(problem) => line.text_color(ui.warning).child(tr(problem)),
                                None => line.child(ui::hint_bar(
                                    &[("⌥⇧↑", tr("move")), ("⇥", tr("message"))],
                                    ui,
                                )),
                            }),
                    )
                    .child(
                        ui::text_button("rebase-cancel", tr("Cancel"), false, ui)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    )
                    .child(
                        ui::primary_button(
                            "rebase-start",
                            tr("Start Rebasing"),
                            problem.is_none(),
                            ui,
                        )
                        .tooltip(ui::tooltip(tr("Start Rebasing"), Some("⌘↵".into())))
                        .when(problem.is_none(), |button| {
                            button.on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.start(window, cx)
                            }))
                        }),
                    ),
            )
    }
}

// --- The message dialog: Edit Commit Message…, Squash Into… ---

type OnMessage = Box<dyn FnOnce(String, &mut Window, &mut App)>;

pub struct MessageDialog {
    title: SharedString,
    subtitle: SharedString,
    confirm_label: SharedString,
    message: Entity<Editor>,
    on_confirm: Option<OnMessage>,
    _subscription: Subscription,
}

impl EventEmitter<DismissEvent> for MessageDialog {}

impl MessageDialog {
    fn new(
        title: impl Into<SharedString>,
        subtitle: impl Into<SharedString>,
        text: &str,
        confirm_label: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let text = text.to_string();
        let message = cx.new(|cx| {
            let mut editor = Editor::message(tr("Commit Message"), window, cx);
            editor.replace_ranges(vec![(0..0, text)], cx);
            editor
                .document
                .set_selection(flux_core::Selection::point(0));
            editor
        });
        let subscription = cx.observe(&message, |_, _, cx| cx.notify());
        window.focus(&message.focus_handle(cx));
        Self {
            title: title.into(),
            subtitle: subtitle.into(),
            confirm_label: confirm_label.into(),
            message,
            on_confirm: None,
            _subscription: subscription,
        }
    }

    fn on_confirm(
        mut self,
        on_confirm: impl FnOnce(String, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_confirm = Some(Box::new(on_confirm));
        self
    }

    fn text(&self, cx: &App) -> String {
        self.message.read(cx).document.text().to_string()
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.text(cx);
        if text.trim().is_empty() {
            return;
        }
        cx.emit(DismissEvent);
        if let Some(on_confirm) = self.on_confirm.take() {
            on_confirm(text, window, cx);
        }
    }
}

impl Focusable for MessageDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.message.focus_handle(cx)
    }
}

impl Render for MessageDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let valid = !self.text(cx).trim().is_empty();
        let focused = self.message.focus_handle(cx).is_focused(window);
        ui::popover(ui)
            .key_context("CommitMessageDialog")
            .on_action(cx.listener(|this, _: &ConfirmMessage, window, cx| this.confirm(window, cx)))
            .on_action(cx.listener(|_, _: &DismissMessage, _, cx| cx.emit(DismissEvent)))
            // Esc in the field (with nothing selected in it) cancels.
            .on_action(cx.listener(|_, _: &crate::editor::Cancel, _, cx| cx.emit(DismissEvent)))
            .w(px(MESSAGE_DIALOG_WIDTH))
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
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(self.title.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.dim)
                            .truncate()
                            .child(self.subtitle.clone()),
                    ),
            )
            .child(
                div()
                    .h(px(MESSAGE_DIALOG_HEIGHT))
                    .pl_1()
                    .pt_1()
                    .rounded(px(RADIUS_MD))
                    .bg(ui.input_background)
                    .border_1()
                    .border_color(if focused {
                        ui.focus_border
                    } else {
                        ui.input_border
                    })
                    .when(focused, |field| field.shadow(ui::focus_ring(ui)))
                    .child(self.message.clone()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .child(ui::hint_bar(&[("⌘↵", tr("confirm"))], ui)),
                    )
                    .child(
                        ui::text_button("message-dialog-cancel", tr("Cancel"), false, ui)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    )
                    .child(
                        ui::primary_button(
                            "message-dialog-ok",
                            self.confirm_label.clone(),
                            valid,
                            ui,
                        )
                        .when(valid, |button| {
                            button.on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.confirm(window, cx)
                            }))
                        }),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(oid: &str, action: RebaseAction) -> Row {
        Row {
            oid: oid.into(),
            summary: format!("{oid} summary"),
            action,
            message: None,
            edited: false,
            full: format!("{oid} message"),
            author: "A".into(),
            time: 0,
        }
    }

    /// Rows newest first: c, b, a.
    fn rows() -> Vec<Row> {
        vec![
            row("c", RebaseAction::Pick),
            row("b", RebaseAction::Pick),
            row("a", RebaseAction::Pick),
        ]
    }

    fn oids(plan: &[RebaseEntry]) -> Vec<&str> {
        plan.iter().map(|entry| entry.oid.as_str()).collect()
    }

    #[test]
    fn the_plan_is_oldest_first() {
        let mut rows = rows();
        set_action(&mut rows, &[0], RebaseAction::Reword);
        let plan = plan_of(&rows);
        assert_eq!(oids(&plan), ["a", "b", "c"]);
        assert_eq!(plan[2].action, RebaseAction::Reword);
        assert_eq!(plan[2].message.as_deref(), Some("c message"));
    }

    #[test]
    fn the_oldest_commit_cannot_meld() {
        let mut rows = rows();
        assert_eq!(problem(&rows), None);
        set_action(&mut rows, &[2], RebaseAction::Fixup);
        assert!(problem(&rows).is_some());
        // Dropped below it: still nothing to meld into.
        let mut rows = rows_with(&[RebaseAction::Squash, RebaseAction::Drop]);
        assert!(problem(&rows).is_some());
        set_action(&mut rows, &[1], RebaseAction::Pick);
        assert_eq!(problem(&rows), None);
    }

    fn rows_with(actions: &[RebaseAction]) -> Vec<Row> {
        actions
            .iter()
            .enumerate()
            .map(|(at, action)| row(&format!("r{at}"), *action))
            .collect()
    }

    #[test]
    fn everything_dropped_is_a_problem() {
        let mut rows = rows();
        set_action(&mut rows, &[0, 1, 2], RebaseAction::Drop);
        assert_eq!(problem(&rows), Some("Every commit is dropped"));
    }

    #[test]
    fn an_empty_reword_is_a_problem() {
        let mut rows = rows();
        set_action(&mut rows, &[1], RebaseAction::Reword);
        rows[1].message = Some("  ".into());
        assert!(problem(&rows).is_some());
    }

    #[test]
    fn a_squash_joins_messages_oldest_first() {
        let mut rows = rows();
        set_action(&mut rows, &[0, 1], RebaseAction::Squash);
        // The newest squash holds the message of the run onto `a`.
        assert_eq!(
            rows[0].message.as_deref(),
            Some("a message\n\nb message\n\nc message")
        );
        assert_eq!(rows[1].message, None);
        // A fixup's message is left out.
        set_action(&mut rows, &[1], RebaseAction::Fixup);
        assert_eq!(rows[0].message.as_deref(), Some("a message\n\nc message"));
        // Back to pick: no squash message.
        set_action(&mut rows, &[0], RebaseAction::Pick);
        assert_eq!(rows[0].message, None);
    }

    #[test]
    fn a_written_squash_message_stays() {
        let mut rows = rows();
        set_action(&mut rows, &[0], RebaseAction::Squash);
        rows[0].message = Some("mine".into());
        rows[0].edited = true;
        set_action(&mut rows, &[1], RebaseAction::Squash);
        assert_eq!(rows[0].message.as_deref(), Some("mine"));
    }

    #[test]
    fn drops_in_a_run_do_not_break_it() {
        let mut rows = rows_with(&[RebaseAction::Squash, RebaseAction::Drop, RebaseAction::Pick]);
        settle_messages(&mut rows);
        assert_eq!(run_of(&rows, 0), Some((2, vec![0])));
        assert_eq!(rows[0].message.as_deref(), Some("r2 message\n\nr0 message"));
    }

    #[test]
    fn rows_move_together() {
        let mut rows = rows();
        assert_eq!(move_rows(&mut rows, &[0], true), None);
        assert_eq!(move_rows(&mut rows, &[0, 1], false), Some(vec![1, 2]));
        assert_eq!(
            rows.iter().map(|row| row.oid.as_str()).collect::<Vec<_>>(),
            ["a", "c", "b"]
        );
        assert_eq!(move_rows(&mut rows, &[1, 2], false), None);
    }

    #[test]
    fn a_row_moves_to_a_place() {
        let mut rows = rows();
        assert_eq!(move_row_to(&mut rows, 0, 3), 2);
        assert_eq!(
            rows.iter().map(|row| row.oid.as_str()).collect::<Vec<_>>(),
            ["b", "a", "c"]
        );
        assert_eq!(move_row_to(&mut rows, 2, 0), 0);
        assert_eq!(
            rows.iter().map(|row| row.oid.as_str()).collect::<Vec<_>>(),
            ["c", "b", "a"]
        );
    }

    fn entry(oid: &str) -> RebaseEntry {
        RebaseEntry {
            oid: oid.into(),
            summary: oid.into(),
            action: RebaseAction::Pick,
            message: None,
        }
    }

    #[test]
    fn melded_commits_follow_their_target() {
        let plan: Vec<_> = ["a", "b", "c", "d"].map(entry).into();
        // d and b into a (oids newest first).
        let melded = meld_plan(&plan, "a", &["d".into(), "b".into()], false, None).unwrap();
        assert_eq!(oids(&melded), ["a", "b", "d", "c"]);
        assert_eq!(melded[1].action, RebaseAction::Fixup);
        assert_eq!(melded[2].action, RebaseAction::Fixup);
        assert_eq!(melded[3].action, RebaseAction::Pick);
        // A squash: its message on the last melded commit.
        let squashed = meld_plan(&plan, "b", &["c".into()], true, Some("both".into())).unwrap();
        assert_eq!(oids(&squashed), ["a", "b", "c", "d"]);
        assert_eq!(squashed[2].action, RebaseAction::Squash);
        assert_eq!(squashed[2].message.as_deref(), Some("both"));
        // A commit that isn't in the plan.
        assert_eq!(meld_plan(&plan, "a", &["x".into()], false, None), None);
    }

    #[test]
    fn dropped_commits_are_marked() {
        let plan: Vec<_> = ["a", "b", "c"].map(entry).into();
        let dropped = drop_plan(&plan, &["b".into()]);
        assert_eq!(
            dropped.iter().map(|entry| entry.action).collect::<Vec<_>>(),
            [RebaseAction::Pick, RebaseAction::Drop, RebaseAction::Pick]
        );
    }
}
