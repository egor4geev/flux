//! Tools for Claude (API 0.5). The manifest's `[[tools]]` declares them (a name, a title, a
//! description for Claude, a JSON schema of the arguments, whether the tool changes something);
//! Flux adds them to its MCP server `flux` in the Claude sessions of the window. A call comes to
//! the window's instance as `Event::ToolCalled(call)`; the plugin answers with [`respond`] — at
//! once or later (after a request):
//!
//! ```ignore
//! Event::ToolCalled(call) if call.tool == "read_chat" => {
//!     let args: ReadChat = match call.parse() {
//!         Ok(args) => args,
//!         Err(err) => return tools::respond(call.id, ToolOutput::error(&err)),
//!     };
//!     tools::respond(call.id, ToolOutput::text(&messages).summary("Backend · team — 50 messages"));
//! }
//! ```
//!
//! A tool that changes something (`write = true`) asks the user first, in any of Claude's
//! permission modes: the plugin shows its card in Claude's chat with [`confirm`], and acts on
//! `Event::ToolConfirmed((call, choice))`. `Event::ToolCancelled(call)` — the call is gone (Claude
//! was interrupted): its card goes, and an answer to it is dropped.

use crate::host::tools as raw;
pub use crate::host::tools::{ConfirmChoice, Confirmation, ToolCall, ToolOutput};

/// Answers a call; an answer to a call that is gone is dropped (with a line in the log).
pub fn respond(call: u64, output: ToolOutput) {
    raw::respond(call, &output)
}

/// Shows the card of a `write` tool's call in Claude's chat; the choice comes as
/// `Event::ToolConfirmed`. Fails for a call that is gone.
pub fn confirm(call: u64, card: &Confirmation) -> Result<(), String> {
    raw::confirm(call, card)
}

impl ToolCall {
    /// The arguments as a `T` (they are a JSON object, as the tool's schema describes); the error
    /// says what's wrong, for `ToolOutput::error`.
    pub fn parse<T: serde::de::DeserializeOwned>(&self) -> Result<T, String> {
        serde_json::from_str(&self.arguments)
            .map_err(|err| format!("The arguments of {} don't fit: {err}", self.tool))
    }
}

impl ToolOutput {
    /// What Claude reads: text, Markdown, JSON.
    pub fn text(text: &str) -> Self {
        ToolOutput {
            text: text.to_string(),
            error: false,
            summary: None,
            detail: None,
            excerpt: None,
        }
    }

    /// The call failed: `text` says why.
    pub fn error(text: &str) -> Self {
        ToolOutput {
            error: true,
            ..Self::text(text)
        }
    }

    /// The step's line in the chat: "Backend · team — 50 messages" (the tool's title by default).
    pub fn summary(mut self, summary: &str) -> Self {
        self.summary = Some(summary.to_string());
        self
    }

    /// Dim on the right of the step's line: "15–17 Nov".
    pub fn detail(mut self, detail: &str) -> Self {
        self.detail = Some(detail.to_string());
        self
    }

    /// Markdown under the line when the step is open: the excerpts Claude read.
    pub fn excerpt(mut self, markdown: &str) -> Self {
        self.excerpt = Some(markdown.to_string());
        self
    }
}

impl Confirmation {
    /// A card: its title ("Reply in «Backend · team»"), what will be done (the message's text),
    /// the primary button ("Send") and the last one ("Don't send").
    pub fn new(title: &str, text: &str, confirm: &str, reject: &str) -> Self {
        Confirmation {
            title: title.to_string(),
            subtitle: None,
            quote: None,
            text: text.to_string(),
            confirm: confirm.to_string(),
            edit: None,
            reject: reject.to_string(),
            hint: None,
        }
    }

    /// Dim after the title: "to Peter".
    pub fn subtitle(mut self, subtitle: &str) -> Self {
        self.subtitle = Some(subtitle.to_string());
        self
    }

    /// A quote over the text: the message answered, by its author.
    pub fn quote(mut self, author: &str, text: &str) -> Self {
        self.quote = Some((author.to_string(), text.to_string()));
        self
    }

    /// A second button ("Edit"): the plugin puts the text where the user edits it.
    pub fn edit(mut self, label: &str) -> Self {
        self.edit = Some(label.to_string());
        self
    }

    /// Dim on the right of the buttons: "Edit puts the text into the chat's field".
    pub fn hint(mut self, hint: &str) -> Self {
        self.hint = Some(hint.to_string());
        self
    }
}
