//! Branch operations and their dialogs, as in JetBrains IDEs: checkout (with the Smart Checkout
//! question when local changes are in the way), New Branch… (from the current branch or a chosen
//! one), Rename…, Delete (with the "not fully merged" question; a notification offers Restore and
//! deleting the tracked branch), Checkout Tag or Revision…, Merge into the current branch, Rebase
//! onto, Checkout and Rebase.
//!
//! The operations run in the git hub (`GitStore`) in the background; the flows here ask what has to
//! be asked (system dialogs) and tell how it went (notifications). An operation that stops on
//! conflicts opens the Conflicts dialog.

use std::collections::HashSet;

use flux_git::{GitError, Outcome, RefKind};
use gpui::{
    Action, App, ClickEvent, Context, DismissEvent, Div, Entity, EventEmitter, FocusHandle,
    Focusable, FontWeight, InteractiveElement, KeyBinding, PromptButton, PromptLevel, Render,
    SharedString, Subscription, Task, WeakEntity, Window, actions, div, prelude::*, px,
};

use crate::git::{self, CheckoutDone, CheckoutMode, CheckoutTarget, GitStore};
use crate::i18n::{tr, trf};
use crate::input::{InputEvent, TextInput};
use crate::input_dialog::InputDialog;
use crate::notifications::Notification;
use crate::theme::{self, Theme};
use crate::ui::{self, CheckState};
use crate::workspace::Workspace;

actions!(new_branch_dialog, [Confirm, Dismiss]);

/// "New Branch 'name'…" of the branches popup: the New Branch dialog with the name filled in.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = git, no_json)]
pub struct NewBranchNamed {
    pub repo: usize,
    pub start: String,
    pub name: String,
}

/// How many files a question lists before "and N more".
const LISTED_FILES: usize = 8;
/// How many unmerged commits the delete question names.
const LISTED_COMMITS: usize = 10;
/// How many prefixes the New Branch dialog offers.
const PREFIXES: usize = 4;
const DIALOG_WIDTH: f32 = 460.;

pub fn init(cx: &mut App) {
    let context = Some("NewBranchDialog");
    cx.bind_keys([
        KeyBinding::new("enter", Confirm, context),
        KeyBinding::new("escape", Dismiss, context),
    ]);
}

/// The flows' window-level actions: `NewBranch`, `NewBranchFrom`, `RenameBranch`,
/// `CheckoutRevision`, `CheckoutRef`, `DeleteRef`, `RestoreRef`, `MergeRef`, `RebaseOnto`,
/// `CheckoutAndRebase`.
pub fn workspace_actions(root: Div, cx: &mut Context<Workspace>) -> Div {
    root.on_action(cx.listener(|this, _: &git::NewBranch, window, cx| {
        let Some(repo) = current_repo(this, cx) else {
            return;
        };
        let start = this
            .git()
            .read(cx)
            .current_branch(repo)
            .unwrap_or_else(|| "HEAD".into());
        new_branch(this, repo, start, None, window, cx)
    }))
    .on_action(
        cx.listener(|this, action: &git::NewBranchFrom, window, cx| {
            new_branch(this, action.repo, action.start.clone(), None, window, cx)
        }),
    )
    .on_action(cx.listener(|this, action: &NewBranchNamed, window, cx| {
        new_branch(
            this,
            action.repo,
            action.start.clone(),
            Some(action.name.clone()),
            window,
            cx,
        )
    }))
    .on_action(cx.listener(|this, action: &git::RenameBranch, window, cx| {
        rename(this, action.repo, action.name.clone(), window, cx)
    }))
    .on_action(cx.listener(|this, _: &git::CheckoutRevision, window, cx| {
        checkout_revision(this, window, cx)
    }))
    .on_action(cx.listener(|this, action: &git::CheckoutRef, window, cx| {
        checkout_ref(
            this,
            action.repo,
            action.name.clone(),
            action.kind,
            window,
            cx,
        )
    }))
    .on_action(cx.listener(|this, action: &git::DeleteRef, window, cx| {
        delete_ref(
            this,
            action.repo,
            action.name.clone(),
            action.kind,
            window,
            cx,
        )
    }))
    .on_action(cx.listener(|this, action: &git::RestoreRef, window, cx| {
        restore_ref(this, action, window, cx)
    }))
    .on_action(cx.listener(|this, action: &git::MergeRef, window, cx| {
        merge(this, action.repo, action.name.clone(), window, cx)
    }))
    .on_action(cx.listener(|this, action: &git::RebaseOnto, window, cx| {
        rebase(this, action.repo, action.onto.clone(), window, cx)
    }))
    .on_action(
        cx.listener(|this, action: &git::CheckoutAndRebase, window, cx| {
            checkout_and_rebase(
                this,
                action.repo,
                action.branch.clone(),
                action.onto.clone(),
                window,
                cx,
            )
        }),
    )
}

// --- Helpers ---

/// The repository the window works with now; without one, a message.
fn current_repo(workspace: &mut Workspace, cx: &mut Context<Workspace>) -> Option<usize> {
    let active = workspace.active_path(cx);
    let repo = workspace.git().read(cx).current_repo(active.as_deref());
    if repo.is_none() {
        workspace.show_message(tr("No Git repository").into(), cx);
    }
    repo
}

/// The current branch of a repository, or the short hash of a detached HEAD.
fn current_label(git: &GitStore, repo: usize) -> String {
    git.repos()
        .get(repo)
        .and_then(|entry| entry.status.branch.label())
        .unwrap_or_else(|| "HEAD".into())
}

fn short(oid: &str) -> String {
    oid.chars().take(7).collect()
}

/// A notification title that names the repository when the window has several.
fn titled(git: &GitStore, repo: usize, title: String) -> String {
    if git.repos().len() > 1 {
        format!("{title} · {}", git.repo_name(repo))
    } else {
        title
    }
}

fn notify(workspace: &Workspace, notification: Notification, cx: &mut App) {
    workspace
        .git()
        .update(cx, |git, cx| git.notify(notification, cx));
}

fn notify_error(workspace: &Workspace, repo: usize, title: &str, error: &GitError, cx: &mut App) {
    workspace.git().update(cx, |git, cx| {
        let title = titled(git, repo, title.to_string());
        git.notify_error(&title, error, cx)
    });
}

/// Paths for a question: the first few, then "and N more".
fn listed(paths: &[String]) -> String {
    let mut lines: Vec<String> = paths.iter().take(LISTED_FILES).cloned().collect();
    if paths.len() > LISTED_FILES {
        lines.push(trf("and {0} more", &[&(paths.len() - LISTED_FILES)]));
    }
    lines.join("\n")
}

/// "origin/feature/x" → "feature/x" (by the repository's remotes; otherwise without the first
/// part).
fn local_name(git: &GitStore, repo: usize, remote_branch: &str) -> String {
    let refs = git.refs(repo);
    if let Some(reference) = refs.remote(remote_branch) {
        return reference.short_name().to_string();
    }
    refs.remotes
        .iter()
        .find_map(|remote| {
            remote_branch
                .strip_prefix(remote.as_str())?
                .strip_prefix('/')
                .map(str::to_string)
        })
        .unwrap_or_else(|| {
            remote_branch
                .split_once('/')
                .map_or(remote_branch, |(_, rest)| rest)
                .to_string()
        })
}

// --- Checkout ---

/// What comes after a checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Then {
    /// A notification of the checkout.
    Report,
    /// "Checkout and Update": the branch moves forward to its remote branch.
    Update(String),
    /// "Checkout and Rebase onto": the branch is rebased onto the given one.
    Rebase(String),
    /// A tag or a commit: HEAD is detached at this commit.
    Detached(String),
}

/// Checks out `name` of `kind` in a repository (the branches popup, a notification): a local
/// branch; a remote one — as the local branch of the same name (moved forward when it only lags
/// behind), or as a new branch tracking it; a tag — detached.
fn checkout_ref(
    workspace: &mut Workspace,
    repo: usize,
    name: String,
    kind: RefKind,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    match kind {
        RefKind::Local => {
            let git = workspace.git().read(cx);
            if git.current_branch(repo).as_deref() == Some(name.as_str()) {
                let title = titled(git, repo, trf("Already on '{0}'", &[&name]));
                return notify(workspace, Notification::info(title), cx);
            }
            run_checkout(
                workspace,
                repo,
                CheckoutTarget::Local(name),
                CheckoutMode::Normal,
                Then::Report,
                window,
                cx,
            )
        }
        RefKind::Remote => checkout_remote(workspace, repo, name, None, window, cx),
        RefKind::Tag => {
            let oid = workspace
                .git()
                .read(cx)
                .refs(repo)
                .tags
                .iter()
                .find(|tag| tag.name == name)
                .map(|tag| tag.oid.clone())
                .unwrap_or_default();
            run_checkout(
                workspace,
                repo,
                CheckoutTarget::Revision(name),
                CheckoutMode::Normal,
                Then::Detached(oid),
                window,
                cx,
            )
        }
    }
}

/// Checkout of a remote branch, then a rebase onto `rebase_onto` if given.
fn checkout_remote(
    workspace: &mut Workspace,
    repo: usize,
    name: String,
    rebase_onto: Option<String>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let git = workspace.git().read(cx);
    let local = local_name(git, repo, &name);
    let refs = git.refs(repo);
    let existing = refs.local(&local).cloned();
    let then = |update: bool| match (&rebase_onto, update) {
        (Some(onto), _) => Then::Rebase(onto.clone()),
        (None, true) => Then::Update(name.clone()),
        (None, false) => Then::Report,
    };
    match existing {
        Some(branch) if branch.current => match rebase_onto {
            Some(onto) => rebase(workspace, repo, onto, window, cx),
            None => {
                let title = titled(git, repo, trf("Already on '{0}'", &[&local]));
                notify(workspace, Notification::info(title), cx)
            }
        },
        // The branch exists: it is checked out, and moved forward when it tracks this remote
        // branch and only lags behind it ("Checkout and Update").
        Some(branch) => {
            let update = branch.upstream.as_deref() == Some(name.as_str())
                && branch.behind > 0
                && branch.ahead == 0;
            run_checkout(
                workspace,
                repo,
                CheckoutTarget::Local(local),
                CheckoutMode::Normal,
                then(update),
                window,
                cx,
            )
        }
        None => run_checkout(
            workspace,
            repo,
            CheckoutTarget::Remote {
                remote_branch: name.clone(),
                local,
            },
            CheckoutMode::Normal,
            then(false),
            window,
            cx,
        ),
    }
}

/// Checks out `target` in a repository; local changes in the way → Smart Checkout / Force Checkout
/// / Don't Checkout; the result in a notification (conflicts of the unstash — the Conflicts
/// dialog).
pub fn checkout(
    workspace: &mut Workspace,
    repo: usize,
    target: CheckoutTarget,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    run_checkout(
        workspace,
        repo,
        target,
        CheckoutMode::Normal,
        Then::Report,
        window,
        cx,
    )
}

/// Checks out a commit (the log's Checkout Revision): HEAD is detached there, as a notification
/// tells; local changes in the way — the Smart Checkout question.
pub fn checkout_detached(
    workspace: &mut Workspace,
    repo: usize,
    oid: String,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    run_checkout(
        workspace,
        repo,
        CheckoutTarget::Revision(oid.clone()),
        CheckoutMode::Normal,
        Then::Detached(oid),
        window,
        cx,
    )
}

fn run_checkout(
    workspace: &mut Workspace,
    repo: usize,
    target: CheckoutTarget,
    mode: CheckoutMode,
    then: Then,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let task = workspace
        .git()
        .update(cx, |git, cx| git.checkout(repo, target.clone(), mode, cx));
    cx.spawn_in(window, async move |this, cx| {
        let result = task.await;
        this.update_in(cx, |this, window, cx| match result {
            Ok(done) => checked_out(this, repo, target, done, then, window, cx),
            Err(err) => checkout_failed(this, repo, target, mode, then, err, window, cx),
        })
        .ok();
    })
    .detach();
}

#[allow(clippy::too_many_arguments)]
fn checkout_failed(
    workspace: &mut Workspace,
    repo: usize,
    target: CheckoutTarget,
    mode: CheckoutMode,
    then: Then,
    error: GitError,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if let Some(blocked) = flux_git::blocked_by(&error) {
        if !blocked.local.is_empty() && mode == CheckoutMode::Normal {
            return ask_smart_checkout(repo, target, blocked.local, then, window, cx);
        }
        if !blocked.untracked.is_empty() {
            return untracked_in_the_way(&blocked.untracked, window, cx);
        }
    }
    notify_error(workspace, repo, tr("Checkout failed"), &error, cx);
}

/// JetBrains' "Git Checkout Problem": the files whose changes are in the way, and what to do.
fn ask_smart_checkout(
    repo: usize,
    target: CheckoutTarget,
    files: Vec<String>,
    then: Then,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let detail = format!(
        "{}\n\n{}",
        listed(&files),
        tr(
            "Smart Checkout stashes them, checks out and brings them back. Force Checkout throws them away."
        )
    );
    let answer = window.prompt(
        PromptLevel::Warning,
        &trf(
            "Your local changes would be overwritten by checkout of '{0}'",
            &[&target.name()],
        ),
        Some(&detail),
        &[
            PromptButton::new(tr("Smart Checkout")),
            PromptButton::new(tr("Force Checkout")),
            PromptButton::cancel(tr("Don’t Checkout")),
        ],
        cx,
    );
    cx.spawn_in(window, async move |this, cx| {
        let mode = match answer.await {
            Ok(0) => CheckoutMode::Smart,
            Ok(1) => CheckoutMode::Force,
            _ => return,
        };
        this.update_in(cx, |this, window, cx| {
            run_checkout(this, repo, target, mode, then, window, cx)
        })
        .ok();
    })
    .detach();
}

/// Untracked files that a checkout or a merge would overwrite: git won't, and neither will Flux.
fn untracked_in_the_way(files: &[String], window: &mut Window, cx: &mut Context<Workspace>) {
    let detail = format!(
        "{}\n\n{}",
        listed(files),
        tr("Move or delete them, then try again.")
    );
    // The answer doesn't matter: the dialog only tells.
    drop(window.prompt(
        PromptLevel::Warning,
        tr("Untracked files would be overwritten"),
        Some(&detail),
        &[PromptButton::ok(tr("OK"))],
        cx,
    ));
}

fn checked_out(
    workspace: &mut Workspace,
    repo: usize,
    target: CheckoutTarget,
    done: CheckoutDone,
    then: Then,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let name = match &target {
        CheckoutTarget::Local(name) | CheckoutTarget::Revision(name) => name.clone(),
        CheckoutTarget::Remote { local, .. } => local.clone(),
    };
    // The stashed changes of a smart checkout came back: cleanly, or with conflicts (they wait to be
    // resolved; the stash stays — git doesn't drop it — until the user drops it). With conflicts in
    // the working tree, a follow-up merge or rebase can't run: the conflicts come first.
    let unstash = done.unstash;
    let conflicted = matches!(unstash, Some((Outcome::Conflicts, _)));
    let restored = matches!(unstash, Some((outcome, _)) if outcome != Outcome::Conflicts);
    match then {
        Then::Update(remote) if !conflicted => {
            let task = workspace
                .git()
                .update(cx, |git, cx| git.merge(repo, &remote, cx));
            let kind = Integration::Update { name, from: remote };
            return integrate(task, repo, kind, window, cx);
        }
        Then::Rebase(onto) if !conflicted => {
            let task = workspace
                .git()
                .update(cx, |git, cx| git.rebase(repo, &onto, None, cx));
            let kind = Integration::Rebase { branch: name, onto };
            return integrate(task, repo, kind, window, cx);
        }
        Then::Detached(oid) => {
            let at = if oid.is_empty() {
                name.clone()
            } else {
                short(&oid)
            };
            let title = titled(
                workspace.git().read(cx),
                repo,
                trf("HEAD is detached at {0}", &[&at]),
            );
            let notification = Notification::warning(title)
                .body(tr(
                    "Commits made here belong to no branch: create one to keep them.",
                ))
                .action(
                    tr("New Branch…"),
                    git::NewBranchFrom {
                        repo,
                        start: "HEAD".into(),
                    },
                )
                .transient();
            notify(workspace, notification, cx);
        }
        _ => {
            let title = titled(
                workspace.git().read(cx),
                repo,
                trf("Checked out '{0}'", &[&name]),
            );
            let mut notification = Notification::success(title);
            if restored {
                notification = notification.body(tr("Local changes were stashed and restored"));
            }
            notify(workspace, notification, cx);
        }
    }
    if let Some((Outcome::Conflicts, oid)) = unstash {
        crate::conflicts_dialog::show_conflicts(workspace, window, cx);
        let title = titled(
            workspace.git().read(cx),
            repo,
            tr("Local changes restored with conflicts").into(),
        );
        let notification = Notification::warning(title)
            .body(tr(
                "The stash is kept: drop it once the conflicts are resolved.",
            ))
            .action(tr("Resolve…"), git::ResolveConflicts)
            .action(tr("Drop Stash"), git::DropStash { repo, oid });
        notify(workspace, notification, cx);
    }
}

// --- Merge and rebase ---

/// What an integration did, for its messages.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Integration {
    /// `name` merged into `into` (the current branch).
    Merge { name: String, into: String },
    /// `branch` rebased onto `onto`.
    Rebase { branch: String, onto: String },
    /// "Checkout and Update": `name` checked out and moved forward to `from`.
    Update { name: String, from: String },
}

/// Merges `name` into the current branch.
fn merge(
    workspace: &mut Workspace,
    repo: usize,
    name: String,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let into = current_label(workspace.git().read(cx), repo);
    let task = workspace
        .git()
        .update(cx, |git, cx| git.merge(repo, &name, cx));
    integrate(task, repo, Integration::Merge { name, into }, window, cx);
}

/// Rebases the current branch onto `onto`.
fn rebase(
    workspace: &mut Workspace,
    repo: usize,
    onto: String,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let branch = current_label(workspace.git().read(cx), repo);
    let task = workspace
        .git()
        .update(cx, |git, cx| git.rebase(repo, &onto, None, cx));
    integrate(task, repo, Integration::Rebase { branch, onto }, window, cx);
}

/// Checks out `branch` and rebases it onto `onto`: a local branch in one go; a remote one is checked
/// out as a local branch first.
fn checkout_and_rebase(
    workspace: &mut Workspace,
    repo: usize,
    branch: String,
    onto: String,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let refs = workspace.git().read(cx).refs(repo);
    if refs.remote(&branch).is_some() && refs.local(&branch).is_none() {
        return checkout_remote(workspace, repo, branch, Some(onto), window, cx);
    }
    let task = workspace.git().update(cx, |git, cx| {
        git.rebase(repo, &onto, Some(branch.clone()), cx)
    });
    integrate(task, repo, Integration::Rebase { branch, onto }, window, cx);
}

/// Waits for a merge or a rebase and tells how it went.
fn integrate(
    task: Task<Result<Outcome, GitError>>,
    repo: usize,
    kind: Integration,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    cx.spawn_in(window, async move |this, cx| {
        let result = task.await;
        this.update_in(cx, |this, window, cx| {
            report_integration(this, repo, kind, result, window, cx)
        })
        .ok();
    })
    .detach();
}

fn report_integration(
    workspace: &mut Workspace,
    repo: usize,
    kind: Integration,
    result: Result<Outcome, GitError>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let outcome = match result {
        Ok(outcome) => outcome,
        Err(error) => {
            if let Some(blocked) = flux_git::blocked_by(&error)
                && !blocked.untracked.is_empty()
            {
                return untracked_in_the_way(&blocked.untracked, window, cx);
            }
            let title = match kind {
                Integration::Merge { .. } => tr("Merge failed"),
                Integration::Rebase { .. } => tr("Rebase failed"),
                Integration::Update { .. } => tr("Update failed"),
            };
            return notify_error(workspace, repo, title, &error, cx);
        }
    };
    if outcome == Outcome::Conflicts {
        crate::conflicts_dialog::show_conflicts(workspace, window, cx);
        let (title, body, abort) = match &kind {
            Integration::Rebase { branch, onto } => (
                trf(
                    "Rebase of '{0}' onto '{1}' stopped on conflicts",
                    &[branch, onto],
                ),
                tr("Resolve the conflicts, then continue the rebase."),
                tr("Abort Rebase"),
            ),
            Integration::Merge { name, into }
            | Integration::Update {
                name: into,
                from: name,
            } => (
                trf(
                    "Merge of '{0}' into '{1}' stopped on conflicts",
                    &[name, into],
                ),
                tr("Resolve the conflicts, then commit the merge."),
                tr("Abort Merge"),
            ),
        };
        let title = titled(workspace.git().read(cx), repo, title);
        let notification = Notification::warning(title)
            .body(body)
            .action(tr("Resolve…"), git::ResolveConflicts)
            .action(abort, git::AbortRepoOperation { repo });
        return notify(workspace, notification, cx);
    }
    let notification = match (&kind, outcome) {
        (Integration::Update { name, from }, _) => Notification::success(trf(
            "Checked out '{0}' and updated it from '{1}'",
            &[name, from],
        )),
        (Integration::Merge { name, into }, Outcome::UpToDate) => {
            Notification::info(tr("Already up to date")).body(trf(
                "'{0}' already has everything from '{1}'",
                &[into, name],
            ))
        }
        (Integration::Merge { name, into }, Outcome::FastForward) => {
            Notification::success(trf("Fast-forwarded '{0}' to '{1}'", &[into, name]))
        }
        (Integration::Merge { name, into }, _) => {
            Notification::success(trf("Merged '{0}' into '{1}'", &[name, into]))
        }
        (Integration::Rebase { branch, onto }, Outcome::UpToDate) => {
            Notification::info(tr("Already up to date"))
                .body(trf("'{0}' is already based on '{1}'", &[branch, onto]))
        }
        (Integration::Rebase { branch, onto }, Outcome::FastForward) => {
            Notification::success(trf("Fast-forwarded '{0}' to '{1}'", &[branch, onto]))
        }
        (Integration::Rebase { branch, onto }, _) => {
            Notification::success(trf("Rebased '{0}' onto '{1}'", &[branch, onto]))
        }
    };
    let title = titled(
        workspace.git().read(cx),
        repo,
        notification.title.to_string(),
    );
    notify(
        workspace,
        Notification {
            title: title.into(),
            ..notification
        },
        cx,
    );
}

// --- New Branch ---

/// The New Branch dialog: `start` is what the branch starts from, `name` a name to begin with.
fn new_branch(
    workspace: &mut Workspace,
    repo: usize,
    start: String,
    name: Option<String>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let git = workspace.git().clone();
    if git.read(cx).repos().get(repo).is_none() {
        return workspace.show_message(tr("No Git repository").into(), cx);
    }
    // From a remote branch, the local branch of the same name is the usual choice.
    let initial = name.unwrap_or_else(|| {
        let refs = git.read(cx).refs(repo);
        match refs.remote(&start) {
            Some(remote) => remote.short_name().to_string(),
            None => String::new(),
        }
    });
    let weak = cx.weak_entity();
    workspace.toggle_modal(window, cx, move |window, cx| {
        NewBranchDialog::new(weak, git, repo, start, &initial, window, cx)
    });
}

/// Creates a branch; one that can't be checked out because of local changes is created, then
/// checked out the smart way (the question).
#[allow(clippy::too_many_arguments)]
fn create_branch(
    workspace: &mut Workspace,
    repo: usize,
    name: String,
    start: String,
    checkout: bool,
    overwrite: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let task = workspace.git().update(cx, |git, cx| {
        git.create_branch(repo, &name, &start, checkout, overwrite, cx)
    });
    cx.spawn_in(window, async move |this, cx| {
        let result = task.await;
        this.update_in(cx, |this, window, cx| match result {
            Ok(()) => {
                let title = if checkout {
                    trf("Checked out new branch '{0}' from '{1}'", &[&name, &start])
                } else {
                    trf("Created branch '{0}' from '{1}'", &[&name, &start])
                };
                let title = titled(this.git().read(cx), repo, title);
                notify(this, Notification::success(title), cx);
            }
            Err(error)
                if checkout
                    && flux_git::blocked_by(&error)
                        .is_some_and(|blocked| !blocked.local.is_empty()) =>
            {
                create_then_checkout(this, repo, name, start, overwrite, window, cx)
            }
            Err(error) => notify_error(this, repo, tr("Branch not created"), &error, cx),
        })
        .ok();
    })
    .detach();
}

fn create_then_checkout(
    workspace: &mut Workspace,
    repo: usize,
    name: String,
    start: String,
    overwrite: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let task = workspace.git().update(cx, |git, cx| {
        git.create_branch(repo, &name, &start, false, overwrite, cx)
    });
    cx.spawn_in(window, async move |this, cx| {
        let result = task.await;
        this.update_in(cx, |this, window, cx| match result {
            Ok(()) => checkout(this, repo, CheckoutTarget::Local(name), window, cx),
            Err(error) => notify_error(this, repo, tr("Branch not created"), &error, cx),
        })
        .ok();
    })
    .detach();
}

/// The most common prefixes of a repository's branches ("feature/", "bugfix/"), at most `limit`.
pub(crate) fn common_prefixes<'a>(
    names: impl Iterator<Item = &'a str>,
    limit: usize,
) -> Vec<String> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for name in names {
        let Some((prefix, _)) = name.split_once('/') else {
            continue;
        };
        let prefix = format!("{prefix}/");
        match counts.iter_mut().find(|(known, _)| *known == prefix) {
            Some((_, count)) => *count += 1,
            None => counts.push((prefix, 1)),
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    counts
        .into_iter()
        .take(limit)
        .map(|(prefix, _)| prefix)
        .collect()
}

/// What is wrong with a new branch's name, if anything: git's rules, an existing branch (fine when
/// overwriting, but never the current one).
pub(crate) fn new_branch_error(
    name: &str,
    existing: &HashSet<String>,
    current: Option<&str>,
    overwrite: bool,
) -> Option<SharedString> {
    if let Err(reason) = flux_git::check_branch_name(name) {
        return Some(tr(reason).to_string().into());
    }
    if existing.contains(name) {
        if current == Some(name) {
            return Some(trf("'{0}' is the current branch", &[&name]).into());
        }
        if !overwrite {
            return Some(trf("Branch '{0}' already exists", &[&name]).into());
        }
    }
    None
}

/// The New Branch dialog, as JetBrains': the name (with the repository's prefixes a click away),
/// "Checkout branch", and "Overwrite existing branch" when the name is taken.
pub struct NewBranchDialog {
    workspace: WeakEntity<Workspace>,
    repo: usize,
    start: String,
    input: Entity<TextInput>,
    checkout: bool,
    overwrite: bool,
    existing: HashSet<String>,
    current: Option<String>,
    /// The repository's name, when the window has several.
    repo_name: Option<String>,
    prefixes: Vec<String>,
    error: Option<SharedString>,
    /// The user typed: errors show from now on.
    edited: bool,
    _subscription: Subscription,
}

impl EventEmitter<DismissEvent> for NewBranchDialog {}

impl NewBranchDialog {
    fn new(
        workspace: WeakEntity<Workspace>,
        git: Entity<GitStore>,
        repo: usize,
        start: String,
        initial: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            let mut input = TextInput::new(tr("Branch name"), cx).code();
            input.set_text(initial, cx);
            input.select_all(cx);
            input
        });
        let subscription = cx.subscribe_in(&input, window, |this, _, event, _, cx| match event {
            InputEvent::Changed => {
                this.edited = true;
                this.validate(cx);
            }
        });
        let git = git.read(cx);
        let refs = git.refs(repo);
        let existing: HashSet<String> = refs
            .local
            .iter()
            .map(|branch| branch.name.clone())
            .collect();
        let prefixes = common_prefixes(
            refs.local.iter().map(|branch| branch.name.as_str()),
            PREFIXES,
        );
        let mut dialog = Self {
            workspace,
            repo,
            start,
            input,
            checkout: true,
            overwrite: false,
            existing,
            current: git.current_branch(repo),
            repo_name: (git.repos().len() > 1).then(|| git.repo_name(repo)),
            prefixes,
            error: None,
            edited: !initial.is_empty(),
            _subscription: subscription,
        };
        dialog.validate(cx);
        dialog
    }

    fn name(&self, cx: &App) -> String {
        self.input.read(cx).text().trim().to_string()
    }

    fn exists(&self, cx: &App) -> bool {
        self.existing.contains(&self.name(cx))
    }

    fn validate(&mut self, cx: &mut Context<Self>) {
        let name = self.name(cx);
        self.error = new_branch_error(
            &name,
            &self.existing,
            self.current.as_deref(),
            self.overwrite,
        );
        cx.notify();
    }

    /// A prefix chip: the name gets that prefix instead of the one it had.
    fn apply_prefix(&mut self, prefix: String, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.name(cx);
        let rest = self
            .prefixes
            .iter()
            .find_map(|known| name.strip_prefix(known.as_str()))
            .unwrap_or(&name)
            .to_string();
        self.input.update(cx, |input, cx| {
            input.set_text(&format!("{prefix}{rest}"), cx)
        });
        window.focus(&self.input.focus_handle(cx));
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.validate(cx);
        if self.error.is_some() {
            self.edited = true;
            return cx.notify();
        }
        let name = self.name(cx);
        let (repo, start, checkout) = (self.repo, self.start.clone(), self.checkout);
        let overwrite = self.overwrite && self.exists(cx);
        cx.emit(DismissEvent);
        self.workspace
            .update(cx, |workspace, cx| {
                create_branch(
                    workspace, repo, name, start, checkout, overwrite, window, cx,
                )
            })
            .ok();
    }
}

// Focus is the field's: nothing else in the dialog takes it, so clicks on the checkboxes and the
// prefixes keep typing in the field.
impl Focusable for NewBranchDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for NewBranchDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let valid = self.error.is_none();
        let error = self.error.clone().filter(|_| self.edited);
        let exists = self.exists(cx);
        let checkbox = |id: &'static str, label: &'static str, on: bool| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap_1p5()
                .cursor_pointer()
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.text_muted)
                .hover(move |style| style.text_color(ui.foreground))
                .child(ui::checkbox((id, 0usize), CheckState::from_bool(on), ui))
                .child(label)
        };
        let prefixes: Vec<_> = self
            .prefixes
            .iter()
            .enumerate()
            .map(|(index, prefix)| {
                let chosen = prefix.clone();
                div()
                    .id(("branch-prefix", index))
                    .h(px(20.))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded(px(10.))
                    .bg(ui.hover)
                    .cursor_pointer()
                    .font_family(theme::code_font())
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.text_muted)
                    .hover(move |style| style.bg(ui.pressed).text_color(ui.foreground))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.apply_prefix(chosen.clone(), window, cx)
                    }))
                    .child(prefix.clone())
            })
            .collect();
        ui::popover(ui)
            .key_context("NewBranchDialog")
            .on_action(cx.listener(|this, _: &Confirm, window, cx| this.confirm(window, cx)))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(DismissEvent)))
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
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(tr("Create New Branch")),
                    )
                    .child(
                        div()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.dim)
                            .child(match &self.repo_name {
                                Some(repo) => {
                                    format!("{} · {repo}", trf("from '{0}'", &[&self.start]))
                                }
                                None => trf("from '{0}'", &[&self.start]),
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(self.input.clone())
                    .when(!prefixes.is_empty(), |field| {
                        field.child(div().flex().flex_wrap().gap_1p5().children(prefixes))
                    })
                    .children(error.map(|error| {
                        div()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.error)
                            .child(error)
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(
                        checkbox("new-branch-checkout", tr("Checkout branch"), self.checkout)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.checkout = !this.checkout;
                                cx.notify();
                            })),
                    )
                    .when(exists, |options| {
                        options.child(
                            checkbox(
                                "new-branch-overwrite",
                                tr("Overwrite existing branch"),
                                self.overwrite,
                            )
                            .on_click(cx.listener(
                                |this, _: &ClickEvent, _, cx| {
                                    this.overwrite = !this.overwrite;
                                    this.validate(cx);
                                },
                            )),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        ui::text_button("new-branch-cancel", tr("Cancel"), false, ui)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    )
                    .child(
                        ui::primary_button("new-branch-create", tr("Create"), valid, ui).when(
                            valid,
                            |button| {
                                button.on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.confirm(window, cx)
                                }))
                            },
                        ),
                    ),
            )
    }
}

// --- Rename ---

/// What is wrong with a new name for `old`, if anything.
pub(crate) fn rename_error(
    name: &str,
    old: &str,
    existing: &HashSet<String>,
) -> Result<(), SharedString> {
    flux_git::check_branch_name(name)
        .map_err(|reason| SharedString::from(tr(reason).to_string()))?;
    if name == old {
        return Err(tr("Enter a new name").into());
    }
    if existing.contains(name) {
        return Err(trf("Branch '{0}' already exists", &[&name]).into());
    }
    Ok(())
}

fn rename(
    workspace: &mut Workspace,
    repo: usize,
    old: String,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let refs = workspace.git().read(cx).refs(repo);
    let has_upstream = refs
        .local(&old)
        .is_some_and(|branch| branch.upstream.is_some());
    let existing: HashSet<String> = refs
        .local
        .iter()
        .map(|branch| branch.name.clone())
        .collect();
    let weak = cx.weak_entity();
    workspace.toggle_modal(window, cx, move |window, cx| {
        let title = trf("Rename '{0}'", &[&old]);
        let mut dialog = InputDialog::new(title, tr("New name"), window, cx)
            .text(&old, cx)
            .confirm_label(tr("Rename"));
        if has_upstream {
            dialog = dialog.checkbox(tr("Unset upstream branch"), false);
        }
        let validate_old = old.clone();
        dialog
            .validate(
                move |name, _, _| rename_error(name, &validate_old, &existing),
                cx,
            )
            .on_confirm(move |new, values, window, cx| {
                let unset = values.first().copied().unwrap_or(false);
                weak.update(cx, |workspace, cx| {
                    rename_branch(workspace, repo, old, new, unset, window, cx)
                })
                .ok();
            })
    });
}

fn rename_branch(
    workspace: &mut Workspace,
    repo: usize,
    old: String,
    new: String,
    unset_upstream: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let task = workspace.git().update(cx, |git, cx| {
        git.rename_branch(repo, &old, &new, unset_upstream, cx)
    });
    cx.spawn_in(window, async move |this, cx| {
        let result = task.await;
        this.update(cx, |this, cx| match result {
            Ok(()) => {
                let title = titled(
                    this.git().read(cx),
                    repo,
                    trf("Renamed '{0}' to '{1}'", &[&old, &new]),
                );
                notify(this, Notification::success(title), cx)
            }
            Err(error) => notify_error(this, repo, tr("Rename failed"), &error, cx),
        })
        .ok();
    })
    .detach();
}

// --- Checkout Tag or Revision ---

fn checkout_revision(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some(repo) = current_repo(workspace, cx) else {
        return;
    };
    let weak = cx.weak_entity();
    workspace.toggle_modal(window, cx, move |window, cx| {
        InputDialog::new(
            tr("Checkout Tag or Revision"),
            tr("A tag, a branch or a commit hash"),
            window,
            cx,
        )
        .confirm_label(tr("Checkout"))
        .validate(
            |text, _, _| {
                if text.is_empty() {
                    Err(tr("Enter a revision").into())
                } else if text.chars().any(char::is_whitespace) {
                    Err(tr("A revision has no spaces").into())
                } else {
                    Ok(())
                }
            },
            cx,
        )
        .on_confirm(move |revision, _, window, cx| {
            weak.update(cx, |workspace, cx| {
                checkout_resolved(workspace, repo, revision, window, cx)
            })
            .ok();
        })
    });
}

/// Checks out a revision once git knows it: a local branch by name stays a branch, anything else
/// detaches HEAD.
fn checkout_resolved(
    workspace: &mut Workspace,
    repo: usize,
    revision: String,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let refs = workspace.git().read(cx).refs(repo);
    if refs.local(&revision).is_some() {
        return checkout_ref(workspace, repo, revision, RefKind::Local, window, cx);
    }
    if refs.remote(&revision).is_some() {
        return checkout_ref(workspace, repo, revision, RefKind::Remote, window, cx);
    }
    let task = workspace
        .git()
        .update(cx, |git, cx| git.resolve_rev(repo, &revision, cx));
    cx.spawn_in(window, async move |this, cx| {
        let result = task.await;
        this.update_in(cx, |this, window, cx| match result {
            Ok(Some(oid)) => run_checkout(
                this,
                repo,
                CheckoutTarget::Revision(revision),
                CheckoutMode::Normal,
                Then::Detached(oid),
                window,
                cx,
            ),
            Ok(None) => {
                let title = titled(
                    this.git().read(cx),
                    repo,
                    trf("Unknown revision '{0}'", &[&revision]),
                );
                notify(this, Notification::error(title).transient(), cx)
            }
            Err(error) => notify_error(this, repo, tr("Checkout failed"), &error, cx),
        })
        .ok();
    })
    .detach();
}

// --- Delete and restore ---

fn delete_ref(
    workspace: &mut Workspace,
    repo: usize,
    name: String,
    kind: RefKind,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    match kind {
        RefKind::Local => delete_branch(workspace, repo, name, false, window, cx),
        RefKind::Remote => ask_delete_remote(workspace, repo, name, window, cx),
        RefKind::Tag => delete_tag(workspace, repo, name, window, cx),
    }
}

fn delete_branch(
    workspace: &mut Workspace,
    repo: usize,
    name: String,
    force: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let git = workspace.git().read(cx);
    if git.current_branch(repo).as_deref() == Some(name.as_str()) {
        let title = titled(git, repo, tr("The current branch can't be deleted").into());
        return notify(
            workspace,
            Notification::warning(title)
                .body(tr("Check out another branch first."))
                .transient(),
            cx,
        );
    }
    let task = workspace
        .git()
        .update(cx, |git, cx| git.delete_branch(repo, &name, force, cx));
    cx.spawn_in(window, async move |this, cx| {
        let result = task.await;
        this.update_in(cx, |this, window, cx| match result {
            Ok(deleted) => {
                let title = titled(
                    this.git().read(cx),
                    repo,
                    trf("Deleted branch '{0}'", &[&deleted.name]),
                );
                let mut notification = Notification::info(title).action(
                    tr("Restore"),
                    git::RestoreRef {
                        repo,
                        name: deleted.name.clone(),
                        oid: deleted.oid.clone(),
                        kind: RefKind::Local,
                    },
                );
                if let Some(upstream) = deleted.upstream {
                    notification = notification.action(
                        tr("Delete Tracked Branch"),
                        git::DeleteRef {
                            repo,
                            name: upstream,
                            kind: RefKind::Remote,
                        },
                    );
                }
                notify(this, notification, cx);
            }
            Err(error) if !force && flux_git::is_not_fully_merged(&error) => {
                ask_force_delete(this, repo, name, window, cx)
            }
            Err(error) => notify_error(this, repo, tr("Branch not deleted"), &error, cx),
        })
        .ok();
    })
    .detach();
}

/// JetBrains' question for a branch with commits found nowhere else: they are listed.
fn ask_force_delete(
    workspace: &mut Workspace,
    repo: usize,
    name: String,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let into = current_label(workspace.git().read(cx), repo);
    let task = workspace
        .git()
        .update(cx, |git, cx| git.unmerged_commits(repo, &name, &into, cx));
    cx.spawn_in(window, async move |this, cx| {
        let commits = task.await.unwrap_or_default();
        let answer = this.update_in(cx, |_, window, cx| {
            let mut lines: Vec<String> = commits
                .iter()
                .take(LISTED_COMMITS)
                .map(|commit| format!("{}  {}", commit.short, commit.summary))
                .collect();
            if commits.len() > LISTED_COMMITS {
                lines.push(trf("and {0} more", &[&(commits.len() - LISTED_COMMITS)]));
            }
            let detail = format!(
                "{}\n\n{}",
                lines.join("\n"),
                tr("Deleting it loses these commits (Restore brings the branch back).")
            );
            window.prompt(
                PromptLevel::Warning,
                &trf(
                    "Branch '{0}' is not fully merged into '{1}'",
                    &[&name, &into],
                ),
                Some(&detail),
                &[
                    PromptButton::new(tr("Delete")),
                    PromptButton::cancel(tr("Cancel")),
                ],
                cx,
            )
        });
        let Ok(answer) = answer else {
            return;
        };
        if answer.await == Ok(0) {
            this.update_in(cx, |this, window, cx| {
                delete_branch(this, repo, name, true, window, cx)
            })
            .ok();
        }
    })
    .detach();
}

fn ask_delete_remote(
    workspace: &mut Workspace,
    repo: usize,
    name: String,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let oid = workspace
        .git()
        .read(cx)
        .refs(repo)
        .remote(&name)
        .map(|branch| branch.oid.clone());
    let answer = window.prompt(
        PromptLevel::Warning,
        &trf("Delete remote branch '{0}'?", &[&name]),
        Some(tr(
            "The branch is deleted on the server, for everyone who works with it.",
        )),
        &[
            PromptButton::new(tr("Delete")),
            PromptButton::cancel(tr("Cancel")),
        ],
        cx,
    );
    cx.spawn_in(window, async move |this, cx| {
        if answer.await != Ok(0) {
            return;
        }
        let task = this.update(cx, |this, cx| {
            this.git()
                .update(cx, |git, cx| git.delete_remote_branch(repo, &name, cx))
        });
        let Ok(task) = task else {
            return;
        };
        let result = task.await;
        this.update(cx, |this, cx| match result {
            Ok(()) => {
                let title = titled(
                    this.git().read(cx),
                    repo,
                    trf("Deleted remote branch '{0}'", &[&name]),
                );
                let mut notification = Notification::info(title);
                if let Some(oid) = oid {
                    notification = notification.action(
                        tr("Restore"),
                        git::RestoreRef {
                            repo,
                            name: name.clone(),
                            oid,
                            kind: RefKind::Remote,
                        },
                    );
                }
                notify(this, notification, cx)
            }
            Err(error) => notify_error(this, repo, tr("Remote branch not deleted"), &error, cx),
        })
        .ok();
    })
    .detach();
}

fn delete_tag(
    workspace: &mut Workspace,
    repo: usize,
    name: String,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let task = workspace
        .git()
        .update(cx, |git, cx| git.delete_tag(repo, &name, cx));
    cx.spawn_in(window, async move |this, cx| {
        let result = task.await;
        this.update(cx, |this, cx| match result {
            Ok(oid) => {
                let title = titled(
                    this.git().read(cx),
                    repo,
                    trf("Deleted tag '{0}'", &[&name]),
                );
                let notification = Notification::info(title).action(
                    tr("Restore"),
                    git::RestoreRef {
                        repo,
                        name: name.clone(),
                        oid,
                        kind: RefKind::Tag,
                    },
                );
                notify(this, notification, cx)
            }
            Err(error) => notify_error(this, repo, tr("Tag not deleted"), &error, cx),
        })
        .ok();
    })
    .detach();
}

fn restore_ref(
    workspace: &mut Workspace,
    action: &git::RestoreRef,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let (repo, name, oid) = (action.repo, action.name.clone(), action.oid.clone());
    let task = workspace.git().update(cx, |git, cx| match action.kind {
        RefKind::Local => git.restore_branch(repo, &name, &oid, cx),
        RefKind::Tag => git.restore_tag(repo, &name, &oid, cx),
        RefKind::Remote => git.restore_remote_branch(repo, &name, &oid, cx),
    });
    let kind = action.kind;
    cx.spawn_in(window, async move |this, cx| {
        let result = task.await;
        this.update(cx, |this, cx| match result {
            Ok(()) => {
                let title = match kind {
                    RefKind::Local => trf("Restored branch '{0}'", &[&name]),
                    RefKind::Remote => trf("Restored remote branch '{0}'", &[&name]),
                    RefKind::Tag => trf("Restored tag '{0}'", &[&name]),
                };
                let title = titled(this.git().read(cx), repo, title);
                notify(this, Notification::success(title), cx)
            }
            Err(error) => notify_error(this, repo, tr("Restore failed"), &error, cx),
        })
        .ok();
    })
    .detach();
}

/// Restoring a deleted remote branch: its commit is pushed back under its name. (A tag comes back
/// through `GitStore::restore_tag`, with the tag object `delete_tag` returned.)
impl GitStore {
    fn restore_remote_branch(
        &mut self,
        repo: usize,
        remote_branch: &str,
        oid: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), GitError>> {
        let (remote_branch, oid) = (remote_branch.to_string(), oid.to_string());
        self.run(repo, tr("Restoring the remote branch…"), cx, move |repo| {
            let remotes = flux_git::remotes(repo)?;
            let (remote, branch) = remotes
                .iter()
                .find_map(|remote| {
                    let branch = remote_branch
                        .strip_prefix(remote.name.as_str())?
                        .strip_prefix('/')?;
                    Some((remote.name.clone(), branch.to_string()))
                })
                .ok_or_else(|| GitError::Failed {
                    command: "git push".into(),
                    message: format!("No remote for {remote_branch}"),
                })?;
            repo.git()
                .arg("push")
                .arg(&remote)
                .arg(format!("{oid}:refs/heads/{branch}"))
                .output()
                .map(drop)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> HashSet<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn new_branch_names_are_checked() {
        let existing = names(&["main", "feature/a"]);
        assert_eq!(
            new_branch_error("feature/b", &existing, Some("main"), false),
            None
        );
        assert!(new_branch_error("", &existing, Some("main"), false).is_some());
        assert!(new_branch_error("a b", &existing, Some("main"), false).is_some());
        // Taken: an error unless overwritten; the current branch never.
        assert!(new_branch_error("feature/a", &existing, Some("main"), false).is_some());
        assert_eq!(
            new_branch_error("feature/a", &existing, Some("main"), true),
            None
        );
        assert!(new_branch_error("main", &existing, Some("main"), true).is_some());
    }

    #[test]
    fn renames_need_a_free_new_name() {
        let existing = names(&["main", "dev"]);
        assert!(rename_error("dev2", "dev", &existing).is_ok());
        assert!(rename_error("dev", "dev", &existing).is_err());
        assert!(rename_error("main", "dev", &existing).is_err());
        assert!(rename_error("bad..name", "dev", &existing).is_err());
    }

    #[test]
    fn prefixes_are_the_most_common_first() {
        let branches = [
            "main",
            "feature/a",
            "feature/b",
            "bugfix/x",
            "jd/2026.1",
            "feature/c",
            "bugfix/y",
        ];
        assert_eq!(
            common_prefixes(branches.iter().copied(), 2),
            ["feature/", "bugfix/"]
        );
        assert!(common_prefixes(["main", "dev"].iter().copied(), 4).is_empty());
    }

    #[test]
    fn long_file_lists_are_cut() {
        let files: Vec<String> = (0..10).map(|n| format!("f{n}.rs")).collect();
        let text = listed(&files);
        assert_eq!(text.lines().count(), LISTED_FILES + 1);
        assert!(text.ends_with("and 2 more"));
    }
}
