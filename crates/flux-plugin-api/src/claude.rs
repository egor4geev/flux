//! Claude Code sessions the plugin opens with its context (API 0.3, the manifest's `claude`
//! permission): a task to discuss — attached, the field takes the keyboard — or to plan — plan
//! mode, the request sent at once.
//!
//! ```ignore
//! claude::session()
//!     .title(&format!("{key}: {summary}"))
//!     .attach(&key, &task_markdown)
//!     .prompt(&tr("Study the task and the code, and propose a plan"))
//!     .send()
//!     .plan_mode()
//!     .start()?;
//! ```

use crate::host::claude::{self as raw, Attachment, SessionSpec};

/// A session being built: [`session`], then the methods, then [`Session::start`].
#[derive(Debug, Clone)]
pub struct Session(SessionSpec);

pub fn session() -> Session {
    Session(SessionSpec {
        title: None,
        attachments: Vec::new(),
        prompt: None,
        send: false,
        plan_mode: false,
    })
}

impl Session {
    /// The session's title: "TRACK-142: Fix the sign-in".
    pub fn title(mut self, title: &str) -> Self {
        self.0.title = Some(title.to_string());
        self
    }

    /// Something the session gets with the first message: a chip with `label`, the `markdown`
    /// in the message.
    pub fn attach(mut self, label: &str, markdown: &str) -> Self {
        self.0.attachments.push(Attachment {
            label: label.to_string(),
            markdown: markdown.to_string(),
        });
        self
    }

    /// The first message: in the field, or sent ([`Session::send`]).
    pub fn prompt(mut self, prompt: &str) -> Self {
        self.0.prompt = Some(prompt.to_string());
        self
    }

    /// Sends the prompt with the attachments at once.
    pub fn send(mut self) -> Self {
        self.0.send = true;
        self
    }

    /// Plan mode: Claude studies and proposes a plan before editing.
    pub fn plan_mode(mut self) -> Self {
        self.0.plan_mode = true;
        self
    }

    /// Opens the session; fails when Claude Code is turned off or not installed, or the
    /// manifest lacks `claude = true`.
    pub fn start(&self) -> Result<(), String> {
        raw::new_session(&self.0)
    }

    /// What it would open.
    pub fn spec(&self) -> &SessionSpec {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_session() {
        let plan = session()
            .title("TRACK-1: Fix")
            .attach("TRACK-1", "# Fix")
            .prompt("Plan it")
            .send()
            .plan_mode();
        let spec = plan.spec();
        assert_eq!(spec.title.as_deref(), Some("TRACK-1: Fix"));
        assert_eq!(spec.attachments[0].label, "TRACK-1");
        assert!(spec.send && spec.plan_mode);
        let discuss = session().attach("TRACK-1", "# Fix");
        assert!(!discuss.spec().send && discuss.spec().prompt.is_none());
    }
}
