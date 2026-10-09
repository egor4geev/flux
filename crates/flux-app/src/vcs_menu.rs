//! The menu of version control operations (⌃V), as the VCS Operations popup of JetBrains IDEs, in
//! its order: Commit, Push, Update Project, Pull, Fetch; Branches, New Branch, Checkout Tag or
//! Revision; Stash, Unstash; what concludes or undoes an operation in progress (Resolve Conflicts,
//! Continue / Skip / Abort); Show Diff, Rollback, Show History (of the file, of the selection) and
//! Annotate of the active file; Show Changes, Refresh — what
//! doesn't apply right now is dimmed.

use flux_git::RepoState;
use gpui::{Context, Div, InteractiveElement, Window, actions};

use crate::commit_panel::confirm_rollback;
use crate::context_menu::{self, ContextMenu};
use crate::git::{self, GitEvent};
use crate::i18n::tr;
use crate::workspace::Workspace;

// In the `git` namespace: the palette shows it as "Git: Rollback File".
actions!(git, [RollbackFile]);

/// Opens the menu (or closes it, when open). Like JetBrains' quick lists, it shows where the work
/// is: under the caret when an editor has focus, otherwise in the middle of the window.
pub fn open(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let anchor = crate::popup::where_the_work_is(workspace, context_menu::MENU_MIN_WIDTH, window, cx);
    let git = workspace.git().read(cx);
    let has_repo = !git.repos().is_empty();
    let has_changes = !git.changes().is_empty();
    let has_conflicts = !git.conflicts().is_empty();
    let states: Vec<RepoState> = git
        .repos_in_progress()
        .into_iter()
        .map(|repo| git.operation(repo).state)
        .collect();
    let rebasing = states.contains(&RepoState::Rebasing);
    let merging = states.contains(&RepoState::Merging);
    let picking = states.contains(&RepoState::CherryPicking);
    let reverting = states.contains(&RepoState::Reverting);
    let active = workspace.active_path(cx);
    let file_status = active.as_deref().and_then(|path| git.status_of(path));
    let file_changed = file_status.is_some();
    let file_tracked = file_status.is_some_and(|status| status != flux_git::FileStatus::Untracked);
    // History and annotations: a file of a repository that isn't new.
    let in_repo = active
        .as_deref()
        .is_some_and(|path| git.repo_index(path).is_some());
    let has_history = in_repo
        && !matches!(
            file_status,
            Some(flux_git::FileStatus::Untracked | flux_git::FileStatus::Added)
        );
    let editor = workspace.active_editor().map(|editor| editor.read(cx));
    let annotated = editor.is_some_and(|editor| editor.blame.is_on());
    let has_selection =
        editor.is_some_and(|editor| !editor.document.selection().primary().is_empty());
    workspace.toggle_modal(window, cx, move |window, cx| {
        let mut menu = ContextMenu::new(window, cx)
            .title(tr("Git"))
            .entry_if(has_repo, tr("Commit…"), git::Commit)
            .entry_if(has_repo, tr("Push…"), git::Push)
            .entry_if(has_repo, tr("Update Project…"), git::UpdateProject)
            .entry_if(has_repo, tr("Pull…"), git::Pull)
            .entry_if(has_repo, tr("Fetch"), git::Fetch)
            .separator()
            .entry_if(has_repo, tr("Branches…"), git::Branches)
            .entry_if(has_repo, tr("New Branch…"), git::NewBranch)
            .entry_if(
                has_repo,
                tr("Checkout Tag or Revision…"),
                git::CheckoutRevision,
            )
            .separator()
            .entry_if(
                has_repo && has_changes,
                tr("Stash Changes…"),
                git::StashChanges,
            )
            .entry_if(has_repo, tr("Unstash Changes…"), git::UnstashChanges);
        // An operation in progress: what resolves, concludes or undoes it.
        if has_conflicts || !states.is_empty() {
            menu = menu.separator().entry_if(
                has_conflicts,
                tr("Resolve Conflicts…"),
                git::ResolveConflicts,
            );
            if rebasing {
                menu = menu
                    .entry_if(
                        !has_conflicts,
                        tr("Continue Rebase"),
                        git::ContinueOperation,
                    )
                    .entry(tr("Skip Commit"), git::SkipCommit)
                    .entry(tr("Abort Rebase"), git::AbortOperation);
            } else if merging {
                menu = menu.entry(tr("Abort Merge"), git::AbortOperation);
            } else if picking {
                menu = menu
                    .entry_if(
                        !has_conflicts,
                        tr("Continue Cherry-Pick"),
                        git::ContinueOperation,
                    )
                    .entry(tr("Abort Cherry-Pick"), git::AbortOperation);
            } else if reverting {
                menu = menu
                    .entry_if(
                        !has_conflicts,
                        tr("Continue Revert"),
                        git::ContinueOperation,
                    )
                    .entry(tr("Abort Revert"), git::AbortOperation);
            }
        }
        menu = menu
            .separator()
            .entry_if(file_changed, tr("Show Diff"), git::ShowDiff)
            .entry_if(file_tracked, tr("Rollback…"), RollbackFile)
            .entry_if(has_history, tr("Show History"), git::ShowFileHistory)
            .entry_if(
                has_history && has_selection,
                tr("Show History for Selection"),
                git::ShowSelectionHistory,
            );
        menu = match annotated {
            true => menu.entry(tr("Close Annotations"), crate::blame::CloseAnnotations),
            false => menu.entry_if(has_history, tr("Annotate with Git Blame"), git::Annotate),
        };
        menu.separator()
            .entry_if(
                has_repo && has_changes,
                tr("Show Changes"),
                git::ToggleCommitWindow,
            )
            .entry_if(has_repo, tr("Refresh"), git::Refresh)
    });
    // A second ⌃V closed the menu: then there is nothing to place.
    workspace.anchor_modal(anchor);
}

/// The menu's own actions, handled by the window.
pub fn workspace_actions(root: Div, cx: &mut Context<Workspace>) -> Div {
    root.on_action(
        cx.listener(|this, _: &RollbackFile, window, cx| rollback_active_file(this, window, cx)),
    )
}

/// Rollback… for the active file: the same question as in the commit window.
fn rollback_active_file(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(path) = workspace.active_path(cx) else {
        return;
    };
    let git = workspace.git().clone();
    let change = git
        .read(cx)
        .changes()
        .into_iter()
        .find(|change| change.path == path);
    let Some(change) = change else {
        let message = tr("The file has no changes");
        return git.update(cx, |git, cx| {
            git.report(GitEvent::Message(message.into()), cx)
        });
    };
    confirm_rollback(git, vec![change], window, cx, |_, _| {});
}
