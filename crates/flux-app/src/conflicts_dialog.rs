//! Conflicts, as JetBrains IDEs resolve them.
//!
//! - **The Conflicts dialog** (`ResolveConflicts`, and right after an operation of the window stopped
//!   on conflicts — [`show_conflicts`]): the conflicted files of every repository with what each
//!   side did, Accept Yours / Accept Theirs for the selected files (several with ⇧ / ⌘), Merge… —
//!   the merge tool for one file. The dialog follows the git status; when nothing is left it says
//!   so and offers what concludes the operation: Commit… for a merge, Continue for a rebase, a
//!   cherry-pick or a revert, and Abort.
//! - **The operation in progress**: Continue (a merge is concluded by a commit instead), Skip Commit
//!   (a rebase) and Abort, each asked about first where something is lost; the simple actions act on
//!   the active file's repository if it is in the middle of one, otherwise on the first that is.
//! - When the last conflict of a repository is resolved (here, or in the merge tool), a
//!   notification suggests the next step.
//!
//! In a rebase git's "ours" is the branch being rebased onto and "theirs" the commit being
//! replayed: the columns and buttons name the sides by what they are ("Upstream", "Yours").

use std::path::PathBuf;

use flux_git::{ConflictKind, ConflictSide, Outcome, RepoState};
use gpui::{
    App, ClickEvent, Context, DismissEvent, Div, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, InteractiveElement, KeyBinding, Render, SharedString, Subscription, WeakEntity,
    Window, actions, div, prelude::*, px,
};

use crate::dialog::Dialog;
use crate::git::{self, Change, GitStore};
use crate::i18n::{tr, trf};
use crate::icons::{IconName, file_icon, icon};
use crate::notifications::Notification;
use crate::theme::{self, Theme};
use crate::ui::{self, RADIUS_SM};
use crate::workspace::Workspace;

actions!(
    conflicts_dialog,
    [
        SelectNext,
        SelectPrevious,
        ExtendNext,
        ExtendPrevious,
        MergeSelected,
        AcceptLeftColumn,
        AcceptRightColumn,
        Dismiss,
    ]
);

const WIDTH: f32 = 760.;
const HEIGHT: f32 = 420.;
const ROW_HEIGHT: f32 = 28.;
/// The column of the sides' changes, and the column of the buttons on the right.
const SIDE_COLUMN: f32 = 104.;
const BUTTONS_WIDTH: f32 = 168.;

pub fn init(cx: &mut App) {
    let context = Some("ConflictsDialog");
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("up", SelectPrevious, context),
        KeyBinding::new("shift-down", ExtendNext, context),
        KeyBinding::new("shift-up", ExtendPrevious, context),
        KeyBinding::new("enter", MergeSelected, context),
        // The first letters of Accept Yours / Accept Theirs (the left and right columns).
        KeyBinding::new("alt-y", AcceptLeftColumn, context),
        KeyBinding::new("alt-t", AcceptRightColumn, context),
        KeyBinding::new("escape", Dismiss, context),
    ]);
}

/// Opens the Conflicts dialog (or closes it, when open).
pub fn open(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let git = workspace.git().clone();
    {
        let git = git.read(cx);
        if git.conflicts().is_empty() && git.repos_in_progress().is_empty() {
            return workspace.show_message(tr("There are no conflicts").into(), cx);
        }
    }
    let this = cx.weak_entity();
    workspace.toggle_dialog(window, cx, move |window, cx| {
        ConflictsDialog::new(git, this, window, cx)
    });
    workspace.dismiss_notifications(&["git::ResolveConflicts"], cx);
}

/// An operation of the window stopped on conflicts (a merge, a rebase, an unstash): the Conflicts
/// dialog opens right away, as in JetBrains IDEs (an open one just follows the status).
pub fn show_conflicts(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    if workspace.modal_is::<ConflictsDialog>() {
        return;
    }
    let git = workspace.git().clone();
    let this = cx.weak_entity();
    workspace.toggle_dialog(window, cx, move |window, cx| {
        ConflictsDialog::new(git, this, window, cx)
    });
}

/// The window-level actions: `ResolveConflicts`, `ContinueOperation`, `AbortOperation`,
/// `SkipCommit`, `ContinueRepoOperation`, `AbortRepoOperation`, `SkipRepoCommit`, `OpenMerge`.
pub fn workspace_actions(root: Div, cx: &mut Context<Workspace>) -> Div {
    root.on_action(cx.listener(|this, action: &git::OpenMerge, window, cx| {
        this.open_merge(action.repo, action.path.clone(), window, cx)
    }))
    .on_action(cx.listener(|this, _: &git::ResolveConflicts, window, cx| open(this, window, cx)))
    .on_action(cx.listener(|this, _: &git::ContinueOperation, window, cx| {
        if let Some(repo) = repo_in_progress(this, cx) {
            continue_operation(this, repo, window, cx)
        }
    }))
    .on_action(cx.listener(|this, _: &git::AbortOperation, window, cx| {
        if let Some(repo) = repo_in_progress(this, cx) {
            abort_operation(this, repo, window, cx)
        }
    }))
    .on_action(cx.listener(|this, _: &git::SkipCommit, window, cx| {
        if let Some(repo) = repo_in_progress(this, cx) {
            skip_commit(this, repo, window, cx)
        }
    }))
    .on_action(
        cx.listener(|this, action: &git::ContinueRepoOperation, window, cx| {
            continue_operation(this, action.repo, window, cx)
        }),
    )
    .on_action(
        cx.listener(|this, action: &git::AbortRepoOperation, window, cx| {
            abort_operation(this, action.repo, window, cx)
        }),
    )
    .on_action(
        cx.listener(|this, action: &git::SkipRepoCommit, window, cx| {
            skip_commit(this, action.repo, window, cx)
        }),
    )
}

// --- The operation in progress ---

/// The repository a simple action (Continue, Abort, Skip) acts on: the active file's, if it is in
/// the middle of an operation, otherwise the first one that is; none — a message.
fn repo_in_progress(workspace: &mut Workspace, cx: &mut Context<Workspace>) -> Option<usize> {
    let active = workspace.active_path(cx);
    let git = workspace.git().read(cx);
    let in_progress = git.repos_in_progress();
    let repo = active
        .as_deref()
        .and_then(|path| git.repo_index(path))
        .filter(|repo| in_progress.contains(repo))
        .or(in_progress.first().copied());
    if repo.is_none() {
        workspace.show_message(tr("No operation in progress").into(), cx);
    }
    repo
}

/// What the operation in progress is, for the dialog and messages: "Merging feature/x into main",
/// "Rebasing main onto origin/main · 2/5"; an unstash that conflicted — "Unstash conflicts".
pub(crate) fn describe(git: &GitStore, repo: usize) -> String {
    let operation = git.operation(repo);
    let (ours, theirs) = git.conflict_sides(repo);
    match operation.state {
        RepoState::Merging => trf("Merging {0} into {1}", &[&theirs, &ours]),
        RepoState::Rebasing => {
            let branch = operation
                .rebase_branch
                .clone()
                .unwrap_or_else(|| tr("HEAD").to_string());
            let text = trf("Rebasing {0} onto {1}", &[&branch, &ours]);
            match operation.step {
                Some((step, total)) => format!("{text} · {step}/{total}"),
                None => text,
            }
        }
        RepoState::CherryPicking => trf("Cherry-picking {0}", &[&theirs]),
        RepoState::Reverting => trf("Reverting {0}", &[&theirs]),
        RepoState::Normal | RepoState::Bisecting => tr("Unstash conflicts").to_string(),
    }
}

/// How the sides are called in a state: (ours, theirs) for the columns, and the buttons that take
/// them.
pub(crate) fn side_names(state: RepoState) -> [&'static str; 4] {
    match state {
        RepoState::Rebasing => [
            tr("Upstream"),
            tr("Yours"),
            tr("Accept Upstream"),
            tr("Accept Yours"),
        ],
        RepoState::Normal | RepoState::Bisecting => [
            tr("Current"),
            tr("Stash"),
            tr("Accept Current"),
            tr("Accept Stash"),
        ],
        RepoState::Merging | RepoState::CherryPicking | RepoState::Reverting => [
            tr("Yours"),
            tr("Theirs"),
            tr("Accept Yours"),
            tr("Accept Theirs"),
        ],
    }
}

/// What each side did to a conflicted file: (ours, theirs).
pub(crate) fn side_changes(kind: Option<ConflictKind>) -> (&'static str, &'static str) {
    match kind {
        None | Some(ConflictKind::BothModified) => (tr("Modified"), tr("Modified")),
        Some(ConflictKind::BothAdded) => (tr("Added"), tr("Added")),
        Some(ConflictKind::AddedByUs) => (tr("Added"), tr("Modified")),
        Some(ConflictKind::AddedByThem) => (tr("Modified"), tr("Added")),
        Some(ConflictKind::DeletedByUs) => (tr("Deleted"), tr("Modified")),
        Some(ConflictKind::DeletedByThem) => (tr("Modified"), tr("Deleted")),
        Some(ConflictKind::BothDeleted) => (tr("Deleted"), tr("Deleted")),
    }
}

/// Whether the merge tool can take a file: both sides have text (one that deletes it can only be
/// taken whole).
pub(crate) fn mergeable(kind: Option<ConflictKind>) -> bool {
    !matches!(
        kind,
        Some(ConflictKind::DeletedByUs | ConflictKind::DeletedByThem | ConflictKind::BothDeleted)
    )
}

/// The files whose merge tool the dialog opened: resolving one brings the dialog back.
#[derive(Default)]
struct FromDialog(std::collections::HashSet<PathBuf>);

impl gpui::Global for FromDialog {}

/// Whether the dialog opened the merge tool of `path` (it should come back once the file is
/// resolved); the mark goes either way.
pub(crate) fn opened_from_dialog(path: &std::path::Path, cx: &mut App) -> bool {
    cx.try_global::<FromDialog>().is_some() && cx.global_mut::<FromDialog>().0.remove(path)
}

/// How many conflicts of a repository are left once `resolved` (relative paths) are.
pub(crate) fn remaining_after(
    git: &Entity<GitStore>,
    repo: usize,
    resolved: &[String],
    cx: &App,
) -> usize {
    git.read(cx)
        .conflicts()
        .iter()
        .filter(|change| change.repo == repo && !resolved.contains(&change.relative))
        .count()
}

/// The last conflict of a repository is resolved: a notification with the step that concludes the
/// operation (Commit for a merge, Continue for a rebase…).
pub(crate) fn notify_resolved(git: &Entity<GitStore>, repo: usize, cx: &mut App) {
    let state = git.read(cx).operation(repo).state;
    let several = git.read(cx).repos().len() > 1;
    let mut notification = Notification::success(tr("All conflicts are resolved"));
    if several {
        notification = notification.body(git.read(cx).repo_name(repo));
    }
    notification = match state {
        RepoState::Merging => notification
            .body(tr("Commit to finish the merge"))
            .action(tr("Commit…"), git::Commit),
        RepoState::Rebasing => notification
            .body(tr("Continue the rebase with the next commit"))
            .action(tr("Continue Rebase"), git::ContinueRepoOperation { repo }),
        RepoState::CherryPicking | RepoState::Reverting => notification
            .body(tr("Continue to finish the operation"))
            .action(tr("Continue"), git::ContinueRepoOperation { repo }),
        RepoState::Normal | RepoState::Bisecting => notification
            .body(tr(
                "The stash is kept: drop it in the Stash tab when you no longer need it",
            ))
            .action(tr("Show Stashes"), git::UnstashChanges),
    };
    git.update(cx, |git, cx| git.notify(notification, cx));
}

/// Continue: a merge is concluded by a commit (the commit window, its message prepared by git);
/// conflicts left — the dialog; otherwise `rebase --continue` and the like.
pub fn continue_operation(
    workspace: &mut Workspace,
    repo: usize,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let git = workspace.git().clone();
    let operation = git.read(cx).operation(repo);
    if operation.state == RepoState::Merging {
        workspace.show_commit_window(true, window, cx);
        return workspace.show_message(tr("Commit to finish the merge").into(), cx);
    }
    if remaining_after(&git, repo, &[], cx) > 0 {
        show_conflicts(workspace, window, cx);
        return workspace.show_message(tr("Resolve the conflicts first").into(), cx);
    }
    if matches!(operation.state, RepoState::Normal | RepoState::Bisecting) {
        return workspace.show_message(tr("No operation in progress").into(), cx);
    }
    let (onto, _) = git.read(cx).conflict_sides(repo);
    let branch = operation.rebase_branch.clone();
    let state = operation.state;
    let task = git.update(cx, |git, cx| git.continue_operation(repo, cx));
    cx.spawn_in(window, async move |this, cx| {
        let result = task.await;
        this.update_in(cx, |this, window, cx| {
            let git = this.git().clone();
            // Without conflicts, a rebase still in progress stopped at the next `edit`.
            let now = git
                .read(cx)
                .repos()
                .get(repo)
                .map(|entry| flux_git::operation(&entry.repo));
            match result {
                Ok(outcome)
                    if outcome != Outcome::Conflicts
                        && state == RepoState::Rebasing
                        && now
                            .as_ref()
                            .is_some_and(|op| op.state == RepoState::Rebasing) =>
                {
                    let at: String = now
                        .and_then(|op| op.stopped_at)
                        .map(|oid| oid.chars().take(8).collect())
                        .unwrap_or_default();
                    let notification =
                        Notification::info(trf("Stopped at {0} for editing", &[&at]))
                            .body(tr(
                                "Amend the commit as you need, then continue the rebase.",
                            ))
                            .action(tr("Continue Rebase"), git::ContinueRepoOperation { repo })
                            .action(tr("Abort Rebase"), git::AbortRepoOperation { repo });
                    git.update(cx, |git, cx| git.notify(notification, cx));
                }
                Ok(Outcome::Conflicts) => {
                    show_conflicts(this, window, cx);
                    stopped_on_conflicts(&git, repo, state, cx);
                }
                Ok(_) => finished(&git, repo, state, branch, onto, cx),
                Err(err) => {
                    let mut notification =
                        Notification::error(tr("Couldn't continue")).body(err.to_string());
                    if state == RepoState::Rebasing {
                        // "Nothing to commit" after taking the upstream's side: the commit is empty.
                        notification =
                            notification.action(tr("Skip Commit"), git::SkipRepoCommit { repo });
                    }
                    if let Some(details) = err.details() {
                        notification = notification.action(
                            tr("Details"),
                            git::ShowGitOutput {
                                title: tr("Couldn't continue").to_string(),
                                output: details.to_string(),
                            },
                        );
                    }
                    git.update(cx, |git, cx| git.notify(notification, cx));
                }
            }
        })
        .ok();
    })
    .detach();
}

/// The operation went on and stopped on conflicts again.
fn stopped_on_conflicts(git: &Entity<GitStore>, repo: usize, state: RepoState, cx: &mut App) {
    let title = match state {
        RepoState::Rebasing => tr("The rebase stopped on conflicts"),
        RepoState::CherryPicking => tr("The cherry-pick stopped on conflicts"),
        RepoState::Reverting => tr("The revert stopped on conflicts"),
        _ => tr("Conflicts"),
    };
    let notification = Notification::warning(title)
        .body(describe(git.read(cx), repo))
        .action(tr("Resolve…"), git::ResolveConflicts)
        .action(tr("Abort"), git::AbortRepoOperation { repo });
    git.update(cx, |git, cx| git.notify(notification, cx));
}

/// Continue or Skip went through: the operation finished — or paused at a step that isn't a
/// conflict (an `edit` of an interactive rebase).
fn finished(
    git: &Entity<GitStore>,
    repo: usize,
    state: RepoState,
    branch: Option<String>,
    onto: String,
    cx: &mut App,
) {
    let still = git
        .read(cx)
        .repos()
        .get(repo)
        .map(|entry| flux_git::operation(&entry.repo).state)
        .unwrap_or_default();
    let notification = if still == state {
        Notification::info(match state {
            RepoState::Rebasing => tr("The rebase paused"),
            _ => tr("The operation paused"),
        })
        .body(describe(git.read(cx), repo))
        .action(tr("Continue"), git::ContinueRepoOperation { repo })
        .action(tr("Abort"), git::AbortRepoOperation { repo })
    } else {
        match state {
            RepoState::Rebasing => {
                let notification = Notification::success(tr("Rebase finished"));
                match branch {
                    Some(branch) => {
                        notification.body(trf("{0} is on top of {1}", &[&branch, &onto]))
                    }
                    None => notification,
                }
            }
            RepoState::CherryPicking => Notification::success(tr("Cherry-pick finished")),
            RepoState::Reverting => Notification::success(tr("Revert finished")),
            _ => Notification::success(tr("Done")),
        }
    };
    git.update(cx, |git, cx| git.notify(notification, cx));
}

/// Abort, after a question: the branch and the working tree go back to where they were.
pub fn abort_operation(
    workspace: &mut Workspace,
    repo: usize,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let git = workspace.git().clone();
    let state = git.read(cx).operation(repo).state;
    let (question, detail, done) = match state {
        RepoState::Merging => (
            tr("Abort the merge?"),
            tr(
                "The branch and the working tree go back to where they were before the merge; local changes stashed for it come back.",
            ),
            tr("Merge aborted"),
        ),
        RepoState::Rebasing => (
            tr("Abort the rebase?"),
            tr(
                "The branch goes back to where it was before the rebase; local changes stashed for it come back.",
            ),
            tr("Rebase aborted"),
        ),
        RepoState::CherryPicking => (
            tr("Abort the cherry-pick?"),
            tr("The branch and the working tree go back to where they were before it."),
            tr("Cherry-pick aborted"),
        ),
        RepoState::Reverting => (
            tr("Abort the revert?"),
            tr("The branch and the working tree go back to where they were before it."),
            tr("Revert aborted"),
        ),
        RepoState::Normal | RepoState::Bisecting => {
            return workspace.show_message(tr("No operation in progress").into(), cx);
        }
    };
    let detail = format!("{}\n\n{detail}", describe(git.read(cx), repo));
    let answer = Dialog::warning(question)
        .message(detail)
        .danger(tr("Abort"))
        .cancel(tr("Cancel"))
        .show(window, cx);
    cx.spawn_in(window, async move |this, cx| {
        if answer.await != Some(0) {
            return;
        }
        let Ok(task) = this.update(cx, |this, cx| {
            this.git()
                .update(cx, |git, cx| git.abort_operation(repo, cx))
        }) else {
            return;
        };
        let result = task.await;
        git.update(cx, |git, cx| match result {
            Ok(()) => git.notify(Notification::info(done), cx),
            Err(err) => git.notify_error(tr("Couldn't abort"), &err, cx),
        })
        .ok();
    })
    .detach();
}

/// Skip Commit (a rebase), after a question: the commit that stopped is left out.
pub fn skip_commit(
    workspace: &mut Workspace,
    repo: usize,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let git = workspace.git().clone();
    let operation = git.read(cx).operation(repo);
    if operation.state != RepoState::Rebasing {
        return workspace.show_message(tr("Only a rebase can skip a commit").into(), cx);
    }
    let commit = operation
        .stopped_at
        .as_deref()
        .map(|oid| oid.chars().take(7).collect::<String>())
        .unwrap_or_default();
    let answer = Dialog::warning(trf("Skip the commit {0}?", &[&commit]))
        .message(tr("Its changes are left out of the rebased branch."))
        .danger(tr("Skip"))
        .cancel(tr("Cancel"))
        .show(window, cx);
    let (onto, _) = git.read(cx).conflict_sides(repo);
    let branch = operation.rebase_branch.clone();
    cx.spawn_in(window, async move |this, cx| {
        if answer.await != Some(0) {
            return;
        }
        let Ok(task) = this.update(cx, |this, cx| {
            this.git().update(cx, |git, cx| git.skip_commit(repo, cx))
        }) else {
            return;
        };
        let result = task.await;
        this.update_in(cx, |this, window, cx| {
            let git = this.git().clone();
            match result {
                Ok(Outcome::Conflicts) => {
                    show_conflicts(this, window, cx);
                    stopped_on_conflicts(&git, repo, RepoState::Rebasing, cx);
                }
                Ok(_) => finished(&git, repo, RepoState::Rebasing, branch, onto, cx),
                Err(err) => git.update(cx, |git, cx| {
                    git.notify_error(tr("Couldn't skip the commit"), &err, cx)
                }),
            }
        })
        .ok();
    })
    .detach();
}

// --- The dialog ---

/// A row of the dialog: a repository's header (with several), or a conflicted file.
#[derive(Debug, Clone, PartialEq)]
enum Row {
    Repo(usize),
    File(Change),
}

/// The rows: the conflicted files by repository and path; a header per repository when the window
/// has several.
fn build_rows(mut conflicts: Vec<Change>, several: bool) -> Vec<Row> {
    conflicts.sort_by(|a, b| (a.repo, &a.relative).cmp(&(b.repo, &b.relative)));
    let mut rows = Vec::new();
    let mut last = None;
    for change in conflicts {
        if several && last != Some(change.repo) {
            rows.push(Row::Repo(change.repo));
            last = Some(change.repo);
        }
        rows.push(Row::File(change));
    }
    rows
}

pub struct ConflictsDialog {
    git: Entity<GitStore>,
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    rows: Vec<Row>,
    /// The selected files (absolute paths), and the one the arrows move from.
    selected: Vec<PathBuf>,
    cursor: Option<PathBuf>,
    /// Where a ⇧-click or ⇧-arrow range starts.
    anchor: Option<PathBuf>,
    /// Accept is running: the buttons wait.
    busy: bool,
    _subscription: Subscription,
}

impl EventEmitter<DismissEvent> for ConflictsDialog {}

impl ConflictsDialog {
    fn new(
        git: Entity<GitStore>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let _ = window;
        let subscription = cx.observe(&git, |this, _, cx| this.rebuild(cx));
        let mut dialog = Self {
            git,
            workspace,
            focus_handle: cx.focus_handle(),
            rows: Vec::new(),
            selected: Vec::new(),
            cursor: None,
            anchor: None,
            busy: false,
            _subscription: subscription,
        };
        dialog.rebuild(cx);
        dialog
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let git = self.git.read(cx);
        let several = git.repos().len() > 1;
        self.rows = build_rows(git.conflicts(), several);
        let files: Vec<PathBuf> = self.files().map(|change| change.path.clone()).collect();
        self.selected.retain(|path| files.contains(path));
        if self
            .cursor
            .as_ref()
            .is_none_or(|cursor| !files.contains(cursor))
        {
            // The selection moves on to the next file, as in JetBrains IDEs.
            self.cursor = self
                .selected
                .first()
                .cloned()
                .or_else(|| files.first().cloned());
        }
        if self.selected.is_empty()
            && let Some(cursor) = &self.cursor
        {
            self.selected.push(cursor.clone());
            self.anchor = Some(cursor.clone());
        }
        cx.notify();
    }

    fn files(&self) -> impl Iterator<Item = &Change> {
        self.rows.iter().filter_map(|row| match row {
            Row::File(change) => Some(change),
            Row::Repo(_) => None,
        })
    }

    fn selected_changes(&self) -> Vec<Change> {
        self.files()
            .filter(|change| self.selected.contains(&change.path))
            .cloned()
            .collect()
    }

    /// The repository whose state names the sides: the selected file's, otherwise the first in
    /// progress.
    fn state(&self, cx: &App) -> RepoState {
        let git = self.git.read(cx);
        let repo = self
            .selected_changes()
            .first()
            .map(|change| change.repo)
            .or_else(|| git.repos_in_progress().first().copied());
        repo.map_or(RepoState::Merging, |repo| git.operation(repo).state)
    }

    fn move_cursor(&mut self, step: isize, extend: bool, cx: &mut Context<Self>) {
        let files: Vec<PathBuf> = self.files().map(|change| change.path.clone()).collect();
        if files.is_empty() {
            return;
        }
        let current = self
            .cursor
            .as_ref()
            .and_then(|cursor| files.iter().position(|path| path == cursor));
        let next = match current {
            Some(at) => (at as isize + step).clamp(0, files.len() as isize - 1) as usize,
            None => 0,
        };
        let path = files[next].clone();
        if extend {
            let anchor = self
                .anchor
                .as_ref()
                .and_then(|anchor| files.iter().position(|path| path == anchor))
                .unwrap_or(next);
            let (from, to) = (anchor.min(next), anchor.max(next));
            self.selected = files[from..=to].to_vec();
        } else {
            self.selected = vec![path.clone()];
            self.anchor = Some(path.clone());
        }
        self.cursor = Some(path);
        cx.notify();
    }

    /// A click on a file: alone; ⌘ — added or taken away; ⇧ — the range from the last click.
    fn click(
        &mut self,
        path: PathBuf,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let modifiers = event.modifiers();
        if event.click_count() == 2 {
            self.selected = vec![path.clone()];
            self.cursor = Some(path);
            return self.merge(window, cx);
        }
        if modifiers.platform {
            if let Some(at) = self.selected.iter().position(|selected| *selected == path) {
                self.selected.remove(at);
            } else {
                self.selected.push(path.clone());
            }
            self.anchor = Some(path.clone());
        } else if modifiers.shift {
            let files: Vec<PathBuf> = self.files().map(|change| change.path.clone()).collect();
            let at = files.iter().position(|file| *file == path).unwrap_or(0);
            let anchor = self
                .anchor
                .as_ref()
                .and_then(|anchor| files.iter().position(|file| file == anchor))
                .unwrap_or(at);
            let (from, to) = (anchor.min(at), anchor.max(at));
            self.selected = files[from..=to].to_vec();
        } else {
            self.selected = vec![path.clone()];
            self.anchor = Some(path.clone());
        }
        self.cursor = Some(path);
        cx.notify();
    }

    /// Accept Yours / Theirs for the selected files, repository by repository.
    fn accept(&mut self, side: ConflictSide, cx: &mut Context<Self>) {
        let changes = self.selected_changes();
        if changes.is_empty() || self.busy {
            return;
        }
        let mut by_repo: Vec<(usize, Vec<String>)> = Vec::new();
        for change in changes {
            match by_repo.iter_mut().find(|(repo, _)| *repo == change.repo) {
                Some((_, paths)) => paths.push(change.relative),
                None => by_repo.push((change.repo, vec![change.relative])),
            }
        }
        self.busy = true;
        let mut tasks = Vec::new();
        for (repo, paths) in by_repo {
            let task = self
                .git
                .update(cx, |git, cx| git.accept_side(repo, paths, side, cx));
            tasks.push(task);
        }
        // The dialog itself says when nothing is left, and offers the next step.
        let git = self.git.clone();
        cx.spawn(async move |this, cx| {
            for task in tasks {
                if let Err(err) = task.await {
                    git.update(cx, |git, cx| {
                        git.notify_error(tr("Couldn't resolve the conflict"), &err, cx)
                    })
                    .ok();
                }
            }
            this.update(cx, |this, cx| {
                this.busy = false;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Merge…: the merge tool for the file at the cursor (one that both sides have).
    fn merge(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let change = self
            .files()
            .find(|change| Some(&change.path) == self.cursor.as_ref())
            .cloned();
        let Some(change) = change.filter(|change| mergeable(change.conflict)) else {
            return;
        };
        // When the file is resolved, the dialog comes back with what is left (JetBrains returns to it).
        cx.default_global::<FromDialog>()
            .0
            .insert(change.path.clone());
        cx.emit(DismissEvent);
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.open_merge(change.repo, change.path, window, cx)
            })
            .ok();
    }

    /// Commit… (a merge with nothing left to resolve): the commit window with the prepared message.
    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_commit_window(true, window, cx)
            })
            .ok();
    }

    fn continue_operation(&mut self, repo: usize, window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
        self.workspace
            .update(cx, |workspace, cx| {
                continue_operation(workspace, repo, window, cx)
            })
            .ok();
    }

    fn abort(&mut self, repo: usize, window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
        self.workspace
            .update(cx, |workspace, cx| {
                abort_operation(workspace, repo, window, cx)
            })
            .ok();
    }

    // --- Rendering ---

    fn render_row(&self, row: &Row, cx: &mut Context<Self>) -> gpui::AnyElement {
        let ui = Theme::ui(cx);
        match row {
            Row::Repo(repo) => {
                let git = self.git.read(cx);
                let branch = git
                    .repos()
                    .get(*repo)
                    .and_then(|entry| entry.status.branch.label())
                    .unwrap_or_default();
                div()
                    .h(px(ROW_HEIGHT))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(git.repo_name(*repo)),
                    )
                    .child(icon(IconName::Branch, ui.violet).size(px(12.)))
                    .child(
                        div()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.violet)
                            .child(branch),
                    )
                    .into_any_element()
            }
            Row::File(change) => {
                let selected = self.selected.contains(&change.path);
                let (dir, name) = match change.relative.rsplit_once('/') {
                    Some((dir, name)) => (Some(dir.to_string()), name.to_string()),
                    None => (None, change.relative.clone()),
                };
                let (ours, theirs) = side_changes(change.conflict);
                let path = change.path.clone();
                let side = |text: &'static str| {
                    let color = if text == tr("Deleted") {
                        ui.vcs_deleted
                    } else if text == tr("Added") {
                        ui.vcs_added
                    } else {
                        ui.vcs_modified
                    };
                    div()
                        .flex_none()
                        .w(px(SIDE_COLUMN))
                        .text_size(px(theme::TEXT_SM))
                        .text_color(color)
                        .child(text)
                };
                div()
                    .id(SharedString::from(
                        change.path.to_string_lossy().into_owned(),
                    ))
                    .h(px(ROW_HEIGHT))
                    .mx_1p5()
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .rounded(px(RADIUS_SM))
                    .cursor_pointer()
                    .when(selected, |row| row.bg(ui.list_selected))
                    .when(!selected, |row| row.hover(move |style| style.bg(ui.hover)))
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        this.click(path.clone(), event, window, cx)
                    }))
                    .child(file_icon(&name, &ui).render().size(px(14.)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .child(div().flex_none().text_color(ui.vcs_conflict).child(name))
                            .children(dir.map(|dir| {
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(theme::TEXT_SM))
                                    .text_color(ui.dim)
                                    .child(dir)
                            })),
                    )
                    .child(side(ours))
                    .child(side(theirs))
                    .into_any_element()
            }
        }
    }

    /// Nothing left to resolve: what concludes the operation.
    fn render_done(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let ui = Theme::ui(cx);
        let git = self.git.read(cx);
        let repo = git.repos_in_progress().first().copied();
        let state = repo.map(|repo| git.operation(repo).state);
        let mut buttons = Vec::new();
        if let (Some(repo), Some(state)) = (repo, state) {
            match state {
                RepoState::Merging => {
                    buttons.push(
                        ui::primary_button("conflicts-commit", tr("Commit…"), true, ui)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.commit(window, cx)
                            }))
                            .into_any_element(),
                    );
                }
                RepoState::Rebasing | RepoState::CherryPicking | RepoState::Reverting => {
                    let label = if state == RepoState::Rebasing {
                        tr("Continue Rebase")
                    } else {
                        tr("Continue")
                    };
                    buttons.push(
                        ui::primary_button("conflicts-continue", label, true, ui)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.continue_operation(repo, window, cx)
                            }))
                            .into_any_element(),
                    );
                }
                RepoState::Normal | RepoState::Bisecting => {}
            }
            if !matches!(state, RepoState::Normal | RepoState::Bisecting) {
                buttons.push(
                    ui::text_button("conflicts-abort", tr("Abort…"), true, ui)
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.abort(repo, window, cx)
                        }))
                        .into_any_element(),
                );
            }
        }
        let hint = match state {
            Some(RepoState::Merging) => Some(tr("Commit to finish the merge")),
            Some(RepoState::Rebasing) => Some(tr("Continue the rebase with the next commit")),
            Some(RepoState::CherryPicking | RepoState::Reverting) => {
                Some(tr("Continue to finish the operation"))
            }
            _ => None,
        };
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .child(icon(IconName::CheckCircle, ui.success).size(px(22.)))
            .child(div().child(tr("All conflicts are resolved")))
            .children(hint.map(|hint| {
                div()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.dim)
                    .child(hint)
            }))
            .when(!buttons.is_empty(), |done| {
                done.child(div().pt_1().flex().gap_2().children(buttons))
            })
            .into_any_element()
    }

    fn render_buttons(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let state = self.state(cx);
        let [_, _, accept_ours, accept_theirs] = side_names(state);
        let any = !self.selected.is_empty() && !self.busy;
        let cursor = self
            .files()
            .find(|change| Some(&change.path) == self.cursor.as_ref());
        let can_merge = !self.busy && cursor.is_some_and(|change| mergeable(change.conflict));
        let keys = |action: &dyn gpui::Action| ui::shortcut_in(action, &self.focus_handle, window);
        let (left_keys, right_keys) = (keys(&AcceptLeftColumn), keys(&AcceptRightColumn));
        let merge_keys = keys(&MergeSelected);
        let button = |id: &'static str, label: &'static str, enabled: bool, keys| {
            ui::text_button(id, label, false, ui)
                .w_full()
                .justify_center()
                .tooltip(ui::tooltip(label, keys))
                .when(!enabled, |button| button.opacity(0.5))
        };
        div()
            .flex_none()
            .w(px(BUTTONS_WIDTH))
            .h_full()
            .border_l_1()
            .border_color(ui.divider)
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                button("conflicts-accept-ours", accept_ours, any, left_keys).when(any, |button| {
                    button.on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.accept(ConflictSide::Ours, cx)
                    }))
                }),
            )
            .child(
                button("conflicts-accept-theirs", accept_theirs, any, right_keys).when(
                    any,
                    |button| {
                        button.on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.accept(ConflictSide::Theirs, cx)
                        }))
                    },
                ),
            )
            .child(
                ui::primary_button("conflicts-merge", tr("Merge…"), can_merge, ui)
                    .w_full()
                    .justify_center()
                    .tooltip(ui::tooltip(tr("Merge…"), merge_keys))
                    .when(can_merge, |button| {
                        button.on_click(
                            cx.listener(|this, _: &ClickEvent, window, cx| this.merge(window, cx)),
                        )
                    }),
            )
    }
}

impl Focusable for ConflictsDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ConflictsDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let git = self.git.read(cx);
        let in_progress = git.repos_in_progress();
        let subtitle = match in_progress.as_slice() {
            [] => self
                .files()
                .next()
                .map(|change| describe(git, change.repo))
                .unwrap_or_default(),
            [repo] => describe(git, *repo),
            [repo, ..] => format!("{}: {}", git.repo_name(*repo), describe(git, *repo)),
        };
        let state = self.state(cx);
        let [ours, theirs, _, _] = side_names(state);
        let count = self.files().count();
        let body = if count == 0 {
            self.render_done(cx)
        } else {
            let rows: Vec<_> = self
                .rows
                .clone()
                .iter()
                .map(|row| self.render_row(row, cx))
                .collect();
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .flex()
                        .flex_col()
                        .child(
                            // Column titles over the files.
                            div()
                                .flex_none()
                                .h(px(26.))
                                .mx_1p5()
                                .px_2()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .text_size(px(theme::TEXT_XS))
                                .text_color(ui.dim)
                                .child(div().flex_1().pl(px(20.)).child(tr("Name")))
                                .child(div().flex_none().w(px(SIDE_COLUMN)).child(ours))
                                .child(div().flex_none().w(px(SIDE_COLUMN)).child(theirs)),
                        )
                        .child(
                            div()
                                .id("conflicts-files")
                                .flex_1()
                                .min_h_0()
                                .pb_2()
                                .overflow_y_scroll()
                                .children(rows),
                        ),
                )
                .child(self.render_buttons(window, cx))
                .into_any_element()
        };
        ui::popover(ui)
            .key_context("ConflictsDialog")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_cursor(1, false, cx)))
            .on_action(
                cx.listener(|this, _: &SelectPrevious, _, cx| this.move_cursor(-1, false, cx)),
            )
            .on_action(cx.listener(|this, _: &ExtendNext, _, cx| this.move_cursor(1, true, cx)))
            .on_action(
                cx.listener(|this, _: &ExtendPrevious, _, cx| this.move_cursor(-1, true, cx)),
            )
            .on_action(cx.listener(|this, _: &MergeSelected, window, cx| this.merge(window, cx)))
            .on_action(
                cx.listener(|this, _: &AcceptLeftColumn, _, cx| {
                    this.accept(ConflictSide::Ours, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &AcceptRightColumn, _, cx| {
                this.accept(ConflictSide::Theirs, cx)
            }))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(DismissEvent)))
            .w(px(WIDTH))
            .h(px(HEIGHT))
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .h(px(52.))
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
                            .child(icon(IconName::Merge, ui.vcs_conflict).size(px(15.)))
                            .child(
                                div()
                                    .flex_none()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(tr("Conflicts")),
                            )
                            .when(count > 0, |title| {
                                title.child(ui::badge(count.to_string(), ui.vcs_conflict))
                            })
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(theme::TEXT_SM))
                                    .text_color(ui.dim)
                                    .child(subtitle),
                            ),
                    )
                    .child(
                        ui::icon_button("conflicts-close", IconName::Close, ui)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    ),
            )
            .child(body)
            .child(
                div()
                    .flex_none()
                    .h(px(44.))
                    .px_4()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_t_1()
                    .border_color(ui.divider)
                    .child(if count > 0 {
                        ui::hint_bar(
                            &[
                                ("↑↓", tr("select")),
                                ("⇧↑↓", tr("several")),
                                ("↵", tr("merge…")),
                                ("esc", tr("close")),
                            ],
                            ui,
                        )
                    } else {
                        ui::hint_bar(&[("esc", tr("close"))], ui)
                    })
                    .child(div().flex_1())
                    .child(
                        ui::text_button("conflicts-close-button", tr("Close"), false, ui)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_git::FileStatus;

    fn change(repo: usize, relative: &str, conflict: Option<ConflictKind>) -> Change {
        Change {
            repo,
            path: PathBuf::from(format!("/r{repo}/{relative}")),
            relative: relative.into(),
            orig_path: None,
            status: FileStatus::Conflicted,
            conflict,
        }
    }

    #[test]
    fn rows_are_grouped_by_repository_when_there_are_several() {
        let conflicts = vec![
            change(1, "b.rs", None),
            change(0, "z.rs", None),
            change(0, "a.rs", None),
        ];
        let rows = build_rows(conflicts.clone(), false);
        let names: Vec<String> = rows
            .iter()
            .map(|row| match row {
                Row::File(change) => change.relative.clone(),
                Row::Repo(repo) => format!("#{repo}"),
            })
            .collect();
        assert_eq!(names, ["a.rs", "z.rs", "b.rs"]);
        let rows = build_rows(conflicts, true);
        let names: Vec<String> = rows
            .iter()
            .map(|row| match row {
                Row::File(change) => change.relative.clone(),
                Row::Repo(repo) => format!("#{repo}"),
            })
            .collect();
        assert_eq!(names, ["#0", "a.rs", "z.rs", "#1", "b.rs"]);
    }

    #[test]
    fn sides_are_named_by_what_they_are() {
        assert_eq!(side_names(RepoState::Merging)[..2], ["Yours", "Theirs"]);
        // In a rebase git's "ours" is the upstream, "theirs" the commit being replayed.
        assert_eq!(side_names(RepoState::Rebasing)[..2], ["Upstream", "Yours"]);
        assert_eq!(side_names(RepoState::Normal)[..2], ["Current", "Stash"]);
        assert_eq!(
            side_changes(Some(ConflictKind::DeletedByThem)),
            ("Modified", "Deleted")
        );
        assert_eq!(side_changes(None), ("Modified", "Modified"));
    }

    #[test]
    fn only_files_both_sides_have_go_to_the_merge_tool() {
        assert!(mergeable(Some(ConflictKind::BothModified)));
        assert!(mergeable(Some(ConflictKind::BothAdded)));
        assert!(mergeable(None));
        assert!(!mergeable(Some(ConflictKind::DeletedByUs)));
        assert!(!mergeable(Some(ConflictKind::DeletedByThem)));
        assert!(!mergeable(Some(ConflictKind::BothDeleted)));
    }
}
