//! Claude for remote work (API 0.6, the manifest's `claude-remote` permission): every Claude
//! session of Flux — in every window, and the unloaded ones of the pult — and what the user could
//! do with them at the computer. For a plugin that brings the sessions to the user elsewhere: a
//! bot in a messenger. The install question warns that the plugin acts for the user.
//!
//! What happens comes as events to the instances that [`watch`] — the service suits best, one for
//! the whole app: `Event::ClaudeSession` (a session appeared or changed), `ClaudeText` (Claude's
//! message as it is written, the whole text so far), `ClaudeSteps`, `ClaudeRequest` (a permission,
//! Claude's questions, the plan), `ClaudeResolved`, `ClaudeTurnEnded`, `ClaudeFailed`,
//! `ClaudeLimits`, `ClaudeRegion`, `SystemChanged`, `ClaudeAction` (the plugin's
//! `[[claude-actions]]` in the pult).
//!
//! ```ignore
//! fn activate(&mut self) {
//!     if service::is_service() {
//!         let _ = remote::watch();
//!     }
//! }
//!
//! fn on_event(&mut self, event: Event) {
//!     match event {
//!         Event::ClaudeRequest((session, request)) => {
//!             // Ask the user elsewhere; then:
//!             let _ = remote::allow(&session, &request.id);
//!         }
//!         Event::ClaudeTurnEnded((session, summary)) => {
//!             let diff = remote::turn_diff(&session, Some(summary.turn));
//!         }
//!         _ => {}
//!     }
//! }
//!
//! let id = remote::new_session("/Users/me/project")
//!     .prompt("Fix the sign-in")
//!     .mode(Mode::Plan)
//!     .start()?;
//! ```

pub use crate::host::claude_remote::{
    Awake, Mode, NewSessionSpec, PermissionRequest, Project, Question, QuestionOption, RegionState,
    Request, RequestKind, Response, RollbackCheck, Session, SessionState, Steps, SystemEvent,
    SystemState, TextUpdate, TranscriptItem, TurnFile, TurnSummary, UsageLimits, UsageWindow,
};

use crate::host::claude_remote as raw;

/// Starts the events for this instance.
pub fn watch() -> Result<(), String> {
    raw::watch(true)
}

/// Stops them.
pub fn unwatch() -> Result<(), String> {
    raw::watch(false)
}

/// Every session: live in any window, and the unloaded ones of the pult.
pub fn sessions() -> Result<Vec<Session>, String> {
    raw::sessions()
}

/// The last `count` messages of a session, oldest first.
pub fn transcript(session: &str, count: u32) -> Result<Vec<TranscriptItem>, String> {
    raw::transcript(session, count)
}

/// Writes to a session; an unloaded one resumes.
pub fn send(session: &str, text: &str) -> Result<(), String> {
    raw::send(session, text, &[])
}

/// [`send`] with pictures: absolute paths in the plugin's data folder.
pub fn send_with_images(session: &str, text: &str, images: &[String]) -> Result<(), String> {
    raw::send(session, text, images)
}

/// Answers a request; fails for one that is gone (answered at the computer meanwhile).
pub fn answer(session: &str, request: &str, response: &Response) -> Result<(), String> {
    raw::answer(session, request, response)
}

/// Allows a permission once.
pub fn allow(session: &str, request: &str) -> Result<(), String> {
    answer(session, request, &Response::Allow(None))
}

/// Allows a permission with one of its `choices` (by id): for the session, always for a rule.
pub fn allow_with(session: &str, request: &str, choice: &str) -> Result<(), String> {
    answer(session, request, &Response::Allow(Some(choice.to_string())))
}

/// Refuses a permission; `message` goes to Claude (empty — Flux's own).
pub fn deny(session: &str, request: &str, message: &str) -> Result<(), String> {
    answer(session, request, &Response::Deny(message.to_string()))
}

/// Stops the turn.
pub fn interrupt(session: &str) -> Result<(), String> {
    raw::interrupt(session)
}

/// Changes the permission mode (`Mode::Other` fails).
pub fn set_mode(session: &str, mode: Mode) -> Result<(), String> {
    raw::set_mode(session, mode)
}

/// The unified diff of a turn; none — the last one.
pub fn turn_diff(session: &str, turn: Option<u32>) -> Result<String, String> {
    raw::turn_diff(session, turn)
}

/// What rolling a turn back would do: ask the user with it, then [`rollback`].
pub fn check_rollback(session: &str, turn: Option<u32>) -> Result<RollbackCheck, String> {
    raw::check_rollback(session, turn)
}

/// Rolls a turn back without asking: the plugin has asked the user.
pub fn rollback(session: &str, turn: Option<u32>) -> Result<(), String> {
    raw::rollback(session, turn)
}

/// The subscription's usage; Flux asks Claude when no session runs.
pub fn limits() -> Result<UsageLimits, String> {
    raw::limits()
}

/// The country check; `check_again` — after the user changed the network.
pub fn region(check_again: bool) -> Result<RegionState, String> {
    raw::region(check_again)
}

/// The recent projects and the open ones.
pub fn projects() -> Result<Vec<Project>, String> {
    raw::projects()
}

/// Keeps the computer awake (or lets it sleep): Flux keeps it awake while any plugin asks.
pub fn set_awake(mode: Awake) -> Result<(), String> {
    raw::set_awake(mode)
}

/// The screen lock and the power adapter now.
pub fn system_status() -> Result<SystemState, String> {
    raw::system_status()
}

/// A new session being built: [`new_session`], then the methods, then [`NewSession::start`].
#[derive(Debug, Clone)]
pub struct NewSession(NewSessionSpec);

/// A new session in `project` (an absolute folder: a recent project from [`projects`]); its
/// window opens in the background when none has it.
pub fn new_session(project: &str) -> NewSession {
    NewSession(NewSessionSpec {
        project: project.to_string(),
        title: None,
        prompt: None,
        attachments: Vec::new(),
        images: Vec::new(),
        mode: Mode::Ask,
    })
}

impl NewSession {
    /// The session's title: "TRACK-142: Fix the sign-in".
    pub fn title(mut self, title: &str) -> Self {
        self.0.title = Some(title.to_string());
        self
    }

    /// The first message, sent at once with the attachments and pictures; without one nothing
    /// is sent.
    pub fn prompt(mut self, prompt: &str) -> Self {
        self.0.prompt = Some(prompt.to_string());
        self
    }

    /// A chip with `label`; Claude reads the `markdown` with the first message.
    pub fn attach(mut self, label: &str, markdown: &str) -> Self {
        self.0
            .attachments
            .push((label.to_string(), markdown.to_string()));
        self
    }

    /// A picture with the first message: an absolute path in the plugin's data folder.
    pub fn image(mut self, path: &str) -> Self {
        self.0.images.push(path.to_string());
        self
    }

    /// The permission mode (Ask by default).
    pub fn mode(mut self, mode: Mode) -> Self {
        self.0.mode = mode;
        self
    }

    /// Opens the session: its id.
    pub fn start(&self) -> Result<String, String> {
        raw::new_session(&self.0)
    }

    /// What it would open.
    pub fn spec(&self) -> &NewSessionSpec {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_new_session() {
        let session = new_session("/p")
            .title("TRACK-1: Fix")
            .attach("TRACK-1", "# Fix")
            .image("/data/a.png")
            .prompt("Plan it")
            .mode(Mode::Plan);
        let spec = session.spec();
        assert_eq!(spec.project, "/p");
        assert_eq!(spec.title.as_deref(), Some("TRACK-1: Fix"));
        assert_eq!(spec.attachments[0].0, "TRACK-1");
        assert_eq!(spec.images, vec!["/data/a.png".to_string()]);
        assert_eq!(spec.mode, Mode::Plan);
        assert_eq!(new_session("/p").spec().mode, Mode::Ask);
    }
}
