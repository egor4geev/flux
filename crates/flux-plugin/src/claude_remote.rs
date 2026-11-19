//! Claude for remote work (API 0.6): what a plugin with the `claude-remote` permission sees of
//! Flux's Claude sessions — all of them, in every window, the unloaded ones of the pult too — and
//! what it may do with them, as plain values between the host (the WIT's `claude-remote`
//! interface) and the app (`flux-app/src/claude_hub.rs`).
//!
//! - A plugin's call goes to the app as [`HostCall::Claude`](crate::runtime::HostCall) — from a
//!   window's instance and from the service alike — and gets one [`RemoteReply`].
//! - What happens in the sessions comes to the instances that watch ([`RemoteCall::Watch`]) as
//!   [`RemoteEvent`]s: the app posts them with [`broadcast`], and a plugin's own pult action with
//!   [`send_to`].
//!
//! The watchers are kept here: an instance joins with `watch(true)` and leaves with `watch(false)`
//! or when its store is gone (its thread ended); the events reach it straight in its queue, as
//! the services' messages do ([`crate::services`]), converted to the WIT's `claude-…` and
//! `system-changed` events.
//!
//! Sessions are named by the CLI's session id (a UUID, the name of the session's transcript file):
//! Flux gives a new session its id before the process starts (`--session-id`), so the id holds from
//! the first moment, across unloading and Flux's restarts.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::runtime::EventSender;

/// A session, as the plugin sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteSession {
    /// The CLI's session id.
    pub id: String,
    /// The project's folder, absolute.
    pub project: String,
    /// Claude's title or the user's; empty until there is one.
    pub title: String,
    pub state: SessionState,
    /// One line on what it does now, as the pult's row says it: "Читает 6 файлов…"; empty when
    /// nothing runs.
    pub activity: String,
    pub mode: RemoteMode,
    /// The window it is open in, by the id `service.windows` gives; none — unloaded.
    pub window: Option<u64>,
    /// When it last did something: seconds since 1970-01-01 UTC.
    pub updated: i64,
    /// What it waits for the user to answer (state `Waiting`).
    pub request: Option<RemoteRequest>,
    /// It finished a turn the user hasn't looked at yet: the pult's green «Готово».
    pub unseen: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// The process is starting.
    Starting,
    /// Claude works on a turn.
    Working,
    /// A question waits for the user: a permission, Claude's questions, the plan.
    Waiting,
    /// Nothing runs: the user's turn.
    Idle,
    /// No process (the pult's unloaded session, or one whose process ended): a message resumes it.
    Unloaded,
}

/// Claude's permission modes, as Flux names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteMode {
    /// «Спрашивать».
    Ask,
    /// «Принимать правки».
    AcceptEdits,
    /// «План».
    Plan,
    /// «Без проверок».
    Bypass,
    /// Another mode of the CLI (refuse whatever asks, the classifier): shown, never set remotely.
    Other,
}

/// A question a session waits on.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteRequest {
    /// The CLI's request id: the answer names it.
    pub id: String,
    pub kind: RequestKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RequestKind {
    /// Allow a tool call: a command, an edit, a fetch, an MCP tool, a path outside the project.
    Permission(PermissionRequest),
    /// Claude's questions with options, answered together.
    Questions(Vec<RemoteQuestion>),
    /// The plan of plan mode, for approval: Markdown.
    Plan(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PermissionRequest {
    /// The title as Flux's card has it: «Claude хочет выполнить команду».
    pub title: String,
    /// The tool: "Bash", "Edit", "WebFetch", "mcp__…".
    pub tool: String,
    /// What the title is about: the command, the file, the URL; none when the title says it all.
    pub subject: Option<String>,
    /// An edit's change: a unified diff, as short as the card's.
    pub diff: Option<String>,
    /// The ways to allow beyond «once» the card offers, by (id, label): «Разрешать в этой сессии»,
    /// «Разрешить и больше не спрашивать: git push».
    pub choices: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RemoteQuestion {
    pub question: String,
    /// A short label (up to 12 characters).
    pub header: String,
    /// By (label, description).
    pub options: Vec<(String, String)>,
    pub multi_select: bool,
}

/// The plugin's answer to a request.
#[derive(Debug, Clone, PartialEq)]
pub enum RemoteAnswer {
    /// Allow: once (none), or with one of the request's `choices` (its id).
    Allow { choice: Option<String> },
    /// Refuse; the message goes to Claude (empty — Flux's own).
    Deny { message: String },
    /// The answers to the questions, in their order: the chosen labels, or the user's own text.
    Answers(Vec<Vec<String>>),
    /// Approve the plan: then accept the edits, or ask for each.
    ApprovePlan { accept_edits: bool },
    /// Don't approve: Claude keeps planning with this feedback.
    KeepPlanning(String),
}

/// What a turn did, at its end.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnSummary {
    /// The turn's number in the session, from 1: `TurnDiff` and the rollback name it.
    pub turn: u32,
    pub files: Vec<TurnFile>,
    /// The turn ended with an error: an API error, a limit, an interrupt.
    pub failed: bool,
    /// Claude's last message of the turn: Markdown.
    pub text: String,
    /// How long it took, in seconds.
    pub seconds: u32,
}

/// A file a turn changed.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnFile {
    /// Relative to the project when it is inside it; absolute otherwise.
    pub path: String,
    pub added: u32,
    pub removed: u32,
    pub created: bool,
}

/// A group of Claude's steps, as the chat groups them: «Прочитал 4 файла · искал env_override».
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteSteps {
    /// The group within the session: the same while the group grows.
    pub group: u64,
    pub summary: String,
    /// One line per step: «src/config.rs», «env_override — 3 файла».
    pub lines: Vec<String>,
    /// The group ended: Claude wrote, asked or finished after it.
    pub done: bool,
}

/// A message of the conversation: what was said last, for a topic's start.
#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptItem {
    /// The user's message (true) or Claude's.
    pub user: bool,
    /// Markdown.
    pub text: String,
}

/// What rolling a turn back would do.
#[derive(Debug, Clone, PartialEq)]
pub struct RollbackCheck {
    /// The files that go back to what they were before the turn.
    pub files: Vec<String>,
    /// Of them, the ones changed on disk since Claude's last edit (by the user, by a later turn):
    /// that goes too.
    pub changed_since: Vec<String>,
    /// The files the turn created: they go to the Trash.
    pub created: Vec<String>,
}

/// The subscription's usage.
#[derive(Debug, Clone, PartialEq)]
pub struct Limits {
    /// The 5-hour window: percent used (0–100), and when it resets (seconds since 1970-01-01 UTC).
    pub five_hour: Option<(u8, i64)>,
    /// The 7-day window, the same way.
    pub seven_day: Option<(u8, i64)>,
    /// A limit is reached: Claude's requests fail until it resets.
    pub rejected: bool,
}

/// The country check (Flux's region guard, ADR-041).
#[derive(Debug, Clone, PartialEq)]
pub enum RegionState {
    /// Claude goes: the country is allowed, or the check is off.
    Allowed,
    /// The check runs: nothing new starts meanwhile.
    Checking,
    /// Claude is blocked: the country (its ISO code; empty — it couldn't be told) and the sentence
    /// Flux shows the user.
    Blocked { country: String, message: String },
}

/// The computer, as remote work needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemState {
    pub screen_locked: bool,
    /// On the power adapter (a desktop is always).
    pub on_power: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemEvent {
    ScreenLocked,
    ScreenUnlocked,
    /// The computer goes to sleep: Flux holds it about two seconds, so the plugin's request goes.
    WillSleep,
    DidWake,
    /// The power source changed: the adapter (true) or the battery.
    Power(bool),
}

/// How the plugin wants the computer awake. Flux keeps it awake while any plugin asks; closing a
/// laptop's lid still puts it to sleep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AwakeMode {
    /// As the system decides.
    #[default]
    Off,
    /// No sleep on its own while Flux runs.
    Always,
    /// The same, only while on the power adapter.
    OnPower,
}

/// A new session.
#[derive(Debug, Clone, PartialEq)]
pub struct NewSession {
    /// The project's folder, absolute: its window opens in the background when none has it.
    pub project: String,
    pub title: Option<String>,
    /// The first message, sent at once with the attachments and images; none — nothing is sent.
    pub prompt: Option<String>,
    /// Chips, by (label, Markdown Claude reads) — as `claude.new-session`'s attachments.
    pub attachments: Vec<(String, String)>,
    /// Pictures: absolute paths (in the plugin's data folder).
    pub images: Vec<String>,
    pub mode: RemoteMode,
}

/// A project Flux knows: a recent one, or one open in a window.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteProject {
    /// Absolute.
    pub path: String,
    /// The folder's name.
    pub name: String,
    /// A window has it open.
    pub open: bool,
}

/// A plugin's call; the reply each kind gets is named after the arrow.
#[derive(Debug, Clone, PartialEq)]
pub enum RemoteCall {
    /// Start (true) or stop getting the events → `Done`.
    Watch(bool),
    /// Every session: live in any window, and the unloaded ones of the pults' «Сейчас» →
    /// `Sessions`.
    Sessions,
    /// The last `count` messages of a session → `Transcript`.
    Transcript { session: String, count: u32 },
    /// A message to the session (an unloaded one resumes; pictures — absolute paths) → `Done`.
    Send {
        session: String,
        text: String,
        images: Vec<String>,
    },
    /// → `Done`; fails for a request that is gone.
    Answer {
        session: String,
        request: String,
        answer: RemoteAnswer,
    },
    /// Stops the turn → `Done`.
    Interrupt { session: String },
    /// → `Done`; `Other` fails.
    SetMode { session: String, mode: RemoteMode },
    /// → `Session` (the new session's id).
    NewSession(NewSession),
    /// The unified diff of a turn (none — the last one) → `Text`.
    TurnDiff { session: String, turn: Option<u32> },
    /// What rolling the turn back would do → `Rollback`.
    CheckRollback { session: String, turn: Option<u32> },
    /// Rolls the turn back without asking — the plugin has asked the user → `Done`.
    Rollback { session: String, turn: Option<u32> },
    /// → `Limits`: the CLI is asked when no session runs.
    Limits,
    /// → `Region`; `again` — check the country again first (the user changed the network).
    Region { again: bool },
    /// The recent projects and the open ones → `Projects`.
    Projects,
    /// → `Done`.
    SetAwake(AwakeMode),
    /// → `System`.
    System,
}

/// The app's reply to a [`RemoteCall`].
#[derive(Debug, Clone, PartialEq)]
pub enum RemoteReply {
    Done,
    /// Why the call failed, for the plugin's log and its user: "No such session".
    Failed(String),
    Sessions(Vec<RemoteSession>),
    Transcript(Vec<TranscriptItem>),
    /// A session's id.
    Session(String),
    Text(String),
    Rollback(RollbackCheck),
    Limits(Limits),
    Region(RegionState),
    Projects(Vec<RemoteProject>),
    System(SystemState),
}

/// What happens, for the instances that watch.
#[derive(Debug, Clone, PartialEq)]
pub enum RemoteEvent {
    /// A session appeared or changed: its state, title, mode, activity, request, `unseen`.
    Session(RemoteSession),
    /// A session left the pult: closed, archived.
    SessionGone(String),
    /// Claude's message as it is written: the whole text so far, about four times a second at
    /// most while it grows; `done` — written to the end. `message` names it within the session.
    Text {
        session: String,
        message: u64,
        text: String,
        done: bool,
    },
    /// A group of steps grew or ended.
    Steps {
        session: String,
        steps: RemoteSteps,
    },
    /// A question to answer (it is in the session's `request` too).
    Request {
        session: String,
        request: RemoteRequest,
    },
    /// The question went: answered in Flux or by a plugin, or withdrawn by Claude.
    Resolved {
        session: String,
        request: String,
    },
    TurnEnded {
        session: String,
        summary: TurnSummary,
    },
    /// The session's process failed: the error, as the chat shows it.
    Failed {
        session: String,
        error: String,
    },
    Limits(Limits),
    Region(RegionState),
    System(SystemEvent),
    /// The user chose the plugin's action at a session in the pult (its `[[claude-actions]]` id).
    /// Only the plugin that declared the action gets it ([`send_to`]).
    Action {
        session: String,
        action: String,
    },
}

/// Posts an event to every instance that watches (`RemoteCall::Watch(true)`), of any plugin with
/// the `claude-remote` permission. Called by the app from any thread; it doesn't wait for the
/// plugins. An [`RemoteEvent::Action`] goes only through [`send_to`]: here it is dropped.
pub fn broadcast(event: RemoteEvent) {
    let Some(event) = crate::host::claude_remote::broadcast_event(event) else {
        return;
    };
    for watcher in watchers().lock().unwrap().iter() {
        watcher.events.send(event.clone());
    }
}

/// Posts an event to the watching instances of one plugin: its own pult action.
pub fn send_to(plugin: &str, event: RemoteEvent) {
    let Some(event) = crate::host::claude_remote::event(event) else {
        return;
    };
    for watcher in watchers().lock().unwrap().iter() {
        if &*watcher.plugin == plugin {
            watcher.events.send(event.clone());
        }
    }
}

/// Some instance watches: the app may skip following what nobody hears (the text as it is
/// written).
pub fn watching() -> bool {
    !watchers().lock().unwrap().is_empty()
}

// --- The watchers ---

/// The instances that watch, in the order they began. Only a plugin built for 0.6 or later with
/// the `claude-remote` permission can join: the host checks the permission, and an older plugin
/// has no `watch` to call.
fn watchers() -> &'static Mutex<Vec<Watcher>> {
    static WATCHERS: OnceLock<Mutex<Vec<Watcher>>> = OnceLock::new();
    WATCHERS.get_or_init(Default::default)
}

/// A watching instance's queue; the token tells it from another instance of the same plugin.
struct Watcher {
    token: u64,
    plugin: Arc<str>,
    events: EventSender,
}

/// An instance's place among the watchers: it leaves when dropped — `watch(false)`, or the
/// instance's store is gone (its thread ended, or it started again on a new project and has to
/// watch again).
pub(crate) struct Watch {
    token: u64,
}

/// The instance begins to watch.
pub(crate) fn watch(plugin: &Arc<str>, events: &EventSender) -> Watch {
    static TOKENS: AtomicU64 = AtomicU64::new(1);
    let token = TOKENS.fetch_add(1, Ordering::Relaxed);
    watchers().lock().unwrap().push(Watcher {
        token,
        plugin: plugin.clone(),
        events: events.clone(),
    });
    Watch { token }
}

impl Drop for Watch {
    fn drop(&mut self) {
        watchers()
            .lock()
            .unwrap()
            .retain(|watcher| watcher.token != self.token);
    }
}
