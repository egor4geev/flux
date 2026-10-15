//! `claude.new-session` (plugin API 0.3, part 10.3): a plugin opens a Claude Code session with its
//! context — "Discuss with Claude" (the task as an attachment chip, the field takes the keyboard)
//! and "Plan with Claude" (plan mode, the request sent at once). The session gets the plugin's
//! title; the attachments go with the first message (each in a tag named by its label, before the
//! text — [`crate::claude_session::attachments_text`]). The chat opens where new sessions do, in
//! the Claude window. The `claude` permission is checked by `flux-plugin`.

use flux_claude::{PermissionMode, UserInput};
use flux_plugin::api::claude::SessionSpec;
use gpui::{Context, Window};

use crate::claude::{self, CliState};
use crate::claude_session::{Attachment, SessionDraft, with_attachments};
use crate::workspace::Workspace;

/// Opens a new Claude session for a plugin. An error says why Claude can't take it (turned off,
/// not installed, signed out); the Claude window then shows the user how to fix it.
pub(crate) fn new_session(
    workspace: &mut Workspace,
    plugin: &str,
    spec: &SessionSpec,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Result<(), String> {
    let _ = plugin;
    if !claude::enabled(cx) {
        return Err("Claude Code is turned off in Settings → Claude Code".into());
    }
    let mode = spec.plan_mode.then_some(PermissionMode::Plan);
    let session = workspace
        .claude
        .update(cx, |store, cx| store.new_session_in_mode(mode, cx));
    let Some(session) = session else {
        let reason = not_ready(workspace.claude.read(cx).cli());
        // The Claude window shows what's wrong and how to fix it (install, sign in).
        window.dispatch_action(Box::new(claude::NewSession), cx);
        return Err(reason.into());
    };
    let attachments: Vec<Attachment> = spec
        .attachments
        .iter()
        .map(|attachment| Attachment {
            label: attachment.label.clone(),
            markdown: attachment.markdown.clone(),
        })
        .collect();
    let title = spec
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_string);
    let prompt = spec
        .prompt
        .clone()
        .filter(|prompt| !prompt.trim().is_empty());
    session.update(cx, |session, cx| {
        if let Some(title) = title {
            session.rename(title, cx);
        }
        if spec.send {
            let text = with_attachments(&attachments, prompt.as_deref().unwrap_or_default());
            if !text.trim().is_empty() {
                session.send(
                    UserInput {
                        text,
                        images: Vec::new(),
                        priority: None,
                    },
                    cx,
                );
            }
        } else {
            // The chat's field takes it when the chat is made (the session was just added).
            session.set_draft(SessionDraft {
                prompt,
                attachments,
            });
        }
    });
    Ok(())
}

/// Why a session can't start: what the plugin hears.
fn not_ready(cli: &CliState) -> &'static str {
    match cli {
        CliState::Missing => "Claude Code isn't installed",
        CliState::SignedOut { .. } => "Claude Code isn't signed in",
        CliState::Failed(_) => "Claude Code doesn't answer",
        CliState::Checking | CliState::Ready { .. } => "Claude Code isn't ready yet",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_why_claude_is_not_ready() {
        assert_eq!(not_ready(&CliState::Missing), "Claude Code isn't installed");
        assert_eq!(
            not_ready(&CliState::Failed("broken".into())),
            "Claude Code doesn't answer"
        );
        assert_eq!(
            not_ready(&CliState::Checking),
            "Claude Code isn't ready yet"
        );
    }

    #[test]
    fn attachments_go_before_the_message() {
        let attachments = [Attachment {
            label: "TRACK-142".into(),
            markdown: "# Fix the \"sign-in\"\n\nIt fails.\n".into(),
        }];
        assert_eq!(
            with_attachments(&attachments, "Plan it"),
            "<attachment name=\"TRACK-142\">\n# Fix the \"sign-in\"\n\nIt fails.\n</attachment>\
             \n\nPlan it"
        );
        assert_eq!(
            with_attachments(&attachments, "  "),
            "<attachment name=\"TRACK-142\">\n# Fix the \"sign-in\"\n\nIt fails.\n</attachment>"
        );
        assert_eq!(with_attachments(&[], "Hi"), "Hi");
    }
}
