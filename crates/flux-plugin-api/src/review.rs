//! Edits the user reviews before they happen: the plugin proposes a file's new text, and Flux shows
//! it as a diff tab — the file as it is on the left, the proposal on the right (editable, hunks
//! rejected one by one), Accept (⌘↵) and Reject — as it shows Claude's edits. The answer comes as
//! `Event::ProposalAnswered((id, outcome))`. Needs the `project` permission.
//!
//! ```ignore
//! let (editor, text) = editor::active_text()?;
//! let path = editor.path?;
//! let fixed = text.replace("\t", "    ");
//! self.proposal = review::proposal(&path, &fixed).title("Tabs to spaces").apply().send().ok();
//! // …
//! Event::ProposalAnswered((id, ProposalOutcome::Accepted(text))) => { /* written, as accepted */ }
//! ```

use crate::host::review as raw;
pub use crate::host::review::ProposalOutcome;

/// A proposed edit: [`proposal`], then the methods add to it.
#[derive(Debug, Clone)]
pub struct Proposal(raw::Proposal);

/// The whole new `text` of the file at `path` (relative to the project root or absolute; it may
/// not exist yet).
pub fn proposal(path: &str, text: &str) -> Proposal {
    Proposal(raw::Proposal {
        path: path.to_string(),
        text: text.to_string(),
        title: None,
        apply: false,
        focus: false,
    })
}

impl Proposal {
    /// The tab's title (the file's name by default).
    pub fn title(mut self, title: &str) -> Self {
        self.0.title = Some(title.to_string());
        self
    }

    /// On Accept Flux writes the accepted text itself: into the open document as one undo step,
    /// saved, or into the file. Without it only the plugin hears the answer.
    pub fn apply(mut self) -> Self {
        self.0.apply = true;
        self
    }

    /// The tab takes the keyboard (by default it opens next to the active one and the keyboard
    /// stays where the user is).
    pub fn focus(mut self) -> Self {
        self.0.focus = true;
        self
    }

    /// Opens the proposal's tab; returns its id.
    pub fn send(&self) -> Result<u64, String> {
        raw::propose(&self.0)
    }

    /// The proposal as the API has it.
    pub fn as_raw(&self) -> &raw::Proposal {
        &self.0
    }
}

/// Closes a proposal's tab without an answer: `Event::ProposalAnswered` with `Closed`.
pub fn withdraw(proposal: u64) {
    raw::withdraw(proposal)
}
