//! An edit Claude proposes, in a diff tab: on the left the file as it is, on the right the proposal
//! (editable), hunks can be rejected one by one; Accept answers the CLI with the user's version,
//! Reject refuses it. The tab closes by itself when the question is answered elsewhere (the card
//! in the chat) or withdrawn. The view is the diff viewer's proposal mode
//! ([`DiffView::proposal`]).

use flux_claude::PendingKind;
use gpui::{App, AppContext, Context, Entity, Window};

use crate::claude_session::ClaudeSession;
use crate::diff_view::DiffView;
use crate::workspace::Workspace;

pub fn init(_cx: &mut App) {}

/// Opens the proposal of the CLI's request `request` in a diff tab (or goes to its tab). An edit
/// that no longer applies isn't opened: its card says why.
///
/// `focus` — the keyboard goes to the diff ("Open Diff" on the card); without it the tab opens next
/// to the active one and the keyboard stays where the user is (a proposal that arrives by itself
/// must not take the ↵ meant for the card).
pub fn open(
    workspace: &mut Workspace,
    session: Entity<ClaudeSession>,
    request: String,
    focus: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let open = workspace
        .diff_views()
        .into_iter()
        .find(|view| view.read(cx).is_proposal_of(&session, &request));
    if let Some(view) = open {
        if focus {
            workspace.activate_diff_view(&view, window, cx);
        }
        return;
    }
    let Some(pending) = session.read(cx).model().pending(&request) else {
        return;
    };
    let PendingKind::Edit(proposal) = &pending.kind else {
        return;
    };
    let Ok(proposed) = &proposal.proposed else {
        return;
    };
    let (path, original, proposed) = (
        proposal.path.clone(),
        proposal.original.clone(),
        proposed.clone(),
    );
    let git = workspace.git().clone();
    let view = cx.new(|cx| {
        DiffView::proposal(path, original, proposed, session, request, git, window, cx)
    });
    workspace.add_diff_view_tab(view, focus, window, cx);
}
