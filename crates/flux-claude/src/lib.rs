//! Claude Code without UI (stage 9): Flux drives the user's own `claude` CLI in host mode — JSON
//! lines in both directions, the same protocol the Agent SDK uses — and the window draws the chat
//! itself. The CLI signs in as the user does (a claude.ai subscription); Flux never asks for an API
//! key. Decision: wiki ADR-030; protocol reference with recorded examples: `PROTOCOL.md` next to
//! this crate.
//!
//! - [`cli`]: where `claude` is, its version, whether the user is signed in, the launch arguments.
//! - [`protocol`]: the frames on the wire (tolerant parsing) and the frames Flux writes.
//! - [`process`]: one `claude` process — reader and writer threads, control requests and answers.
//! - [`session`]: the conversation as the chat shows it — entries, status, the user's pending
//!   questions, tasks, limits; built from the frames, no I/O.
//! - [`edits`]: what an edit Claude proposes would make of a file, and the answer for the user's
//!   (possibly changed) version.
//! - [`transcript`]: past sessions of a project from `~/.claude/projects` (part 9.2).
//! - [`mcp`]: Flux's own MCP server for Claude — the tools of the window (part 9.2).

pub mod cli;
pub mod edits;
pub mod mcp;
pub mod process;
pub mod protocol;
pub mod session;
pub mod transcript;
mod types;

pub use cli::{AuthStatus, Cli, LaunchOptions};
pub use mcp::{McpReply, McpServer, ToolOutput, ToolSpec};
pub use process::{Process, ProcessEvent};
pub use transcript::SavedSession;
pub use protocol::{CliRequest, HostRequest, Incoming};
pub use session::{
    Activity, Answer, Change, ChangedFile, Entry, EntryId, EntryKind, Notice, Pending, PendingKind, Session,
    SessionInfo, Status, TaskItem, TaskStatus, ToolCall, ToolEntry, ToolResult, ToolState,
};
pub use types::{
    BackgroundTask, ContextUsage, Effort, ImageAttachment, LimitStatus, LimitWindow, ModelInfo,
    PermissionMode, Priority, RateLimits, SlashCommand, UserInput,
};
