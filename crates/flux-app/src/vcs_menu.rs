//! The menu of version control operations (⌃V), as the VCS Operations popup of JetBrains IDEs:
//! Commit, Push, Show Diff and Rollback of the active file, Refresh — what doesn't apply right now
//! is dimmed.

use gpui::{App, Context, Div, InteractiveElement, Pixels, Point, Window, actions, point, px};

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
    let anchor = caret_anchor(workspace, window, cx).unwrap_or_else(|| window_middle(window));
    let git = workspace.git().read(cx);
    let has_repo = !git.repos().is_empty();
    let has_changes = !git.changes().is_empty();
    let active = workspace.active_path(cx);
    let file_status = active.as_deref().and_then(|path| git.status_of(path));
    let file_changed = file_status.is_some();
    let file_tracked = file_status.is_some_and(|status| status != flux_git::FileStatus::Untracked);
    workspace.toggle_modal(window, cx, move |window, cx| {
        ContextMenu::new(window, cx)
            .title(tr("Git"))
            .entry_if(has_repo, tr("Commit…"), git::Commit)
            .entry_if(has_repo, tr("Push…"), git::Push)
            .separator()
            .entry_if(file_changed, tr("Show Diff"), git::ShowDiff)
            .entry_if(file_tracked, tr("Rollback…"), RollbackFile)
            .separator()
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

/// Under the caret of the focused editor, if it is on screen.
fn caret_anchor(workspace: &Workspace, window: &Window, cx: &App) -> Option<Point<Pixels>> {
    let editor = workspace.active_editor()?;
    let editor = editor.read(cx);
    if !editor.focus_handle.is_focused(window) {
        return None;
    }
    let head = editor.document.selection().primary().head;
    let anchor = crate::popup::anchor(editor, head)?;
    Some(point(anchor.x, anchor.line_bottom + px(crate::popup::POPUP_GAP)))
}

/// The middle of the window, a third of the way down: where a menu without a caret reads best.
fn window_middle(window: &Window) -> Point<Pixels> {
    let size = window.viewport_size();
    point(
        size.width / 2. - px(context_menu::MENU_MIN_WIDTH / 2.),
        size.height / 3.,
    )
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
