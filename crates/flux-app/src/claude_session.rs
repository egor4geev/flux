//! One Claude Code conversation of the window: the `claude` process in host mode and the session
//! model built from its frames ([`flux_claude::Session`]). The chat reads the model and calls the
//! methods here; what changed comes out as [`SessionEvent`]s.
//!
//! The process starts with the session (the commands and the models come with `initialize`, the
//! subscription's limits with `get_usage`) and again with `--resume` when the user writes after it
//! exited. Every request of the CLI is answered here exactly once: permissions wait for the user
//! (the chat answers them through [`ClaudeSession::answer`]), Flux's tools wait for the window
//! ([`SessionEvent::ToolCall`] → [`ClaudeSession::answer_tool`]), the rest at once.
//!
//! Part 9.2: a saved session comes back in a tab without a process ([`ClaudeSession::resumed`]:
//! the conversation from its transcript); the process starts with `--resume` when the chat is in
//! sight ([`ClaudeSession::ensure_started`]) or the user writes. Flux's MCP server (`flux`, the
//! tools of [`crate::claude_tools`]) is registered in `initialize`; its tools run without a
//! permission card (a PreToolUse hook allows them).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use futures::StreamExt;
use gpui::{AppContext, AsyncApp, Context, EventEmitter, SharedString, Task, WeakEntity};
use serde_json::{Value, json};

use flux_claude::mcp::{self, McpReply, McpServer, ToolOutput};
use flux_claude::process::ProcessEvent;
use flux_claude::protocol::{CliRequest, HostRequest, Incoming};
use flux_claude::session::EDIT_HOOK;
use flux_claude::{
    Answer, Change, Cli, Effort, LaunchOptions, PermissionMode, Process, Session, Status,
    UserInput,
};

use crate::i18n::tr;

/// How often `get_usage` is asked again while the CLI hasn't fetched the limits yet (right after
/// it starts it answers `rate_limits: null`).
const USAGE_ATTEMPTS: usize = 4;
const USAGE_RETRY: Duration = Duration::from_secs(3);
/// How long an edit's hook waits for the window to say what the language server found: then the
/// CLI gets the answer without it (Claude waits for the hook).
const EDIT_HOOK_WAIT: Duration = Duration::from_secs(4);

/// What changed in the session.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionEvent {
    /// The conversation or the session's state changed: redraw.
    Changed,
    /// Idle, working, waiting for the user: the tab's dot, the launchpad, the status bar.
    Status,
    /// A question for the user arrived (by the CLI's request id): a card; an edit — maybe a diff
    /// tab; a notification when the chat isn't in sight.
    PendingAdded(String),
    /// The question went (answered here or in a diff tab, withdrawn by the CLI).
    PendingRemoved(String),
    /// A turn ended.
    TurnFinished {
        is_error: bool,
    },
    /// Nothing runs any more.
    Idle,
    /// Claude changed these files on disk: open documents reload.
    FilesChanged(Vec<std::path::PathBuf>),
    Title,
    Limits,
    /// The process ended.
    Exited,
    /// Claude calls a Flux tool (`mcp__flux__<tool>`): the window runs it and answers with
    /// [`ClaudeSession::answer_tool`] under the CLI's request id.
    ToolCall {
        request: String,
        tool: String,
        input: Value,
    },
    /// An edit of Claude ran on this file: the window tells Claude what language servers now find
    /// wrong in it, or nothing ([`ClaudeSession::answer_edit_hook`] under the CLI's request id).
    EditHook {
        request: String,
        path: PathBuf,
    },
}

/// The PreToolUse hook that lets Flux's own tools run without a permission card.
const TOOLS_HOOK: &str = "flux-tools";

pub struct ClaudeSession {
    model: Session,
    cli: Cli,
    options: LaunchOptions,
    process: Option<Process>,
    _pump: Option<Task<()>>,
    /// Flux's MCP server: answers the CLI's handshake and tool list, hands the calls to the window.
    mcp: McpServer,
    /// Tool calls the window runs: the CLI's request id → the JSON-RPC id of the call.
    tool_calls: HashMap<String, Value>,
    /// Edit hooks waiting for the window ([`SessionEvent::EditHook`]): the CLI's request id → the
    /// answer of the session model, which the problems join.
    edit_hooks: HashMap<String, Value>,
    /// A resumed session's transcript is being read.
    loading: bool,
    _load: Option<Task<()>>,
}

impl EventEmitter<SessionEvent> for ClaudeSession {}

impl ClaudeSession {
    pub fn new(cli: Cli, options: LaunchOptions, cx: &mut Context<Self>) -> Self {
        let mut session = Self::idle(cli, options);
        session.start(cx);
        session
    }

    /// A saved session back in a tab (the history's Resume, Flux restarting): its conversation is
    /// read from the transcript in the background; no process runs until the chat is in sight
    /// ([`Self::ensure_started`]) or the user writes — then `claude --resume <id>`.
    /// `options.resume` names the session.
    pub fn resumed(
        cli: Cli,
        options: LaunchOptions,
        transcript: PathBuf,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut session = Self::idle(cli, options);
        session.model.info.session_id = session.options.resume.clone();
        session.model.status = Status::Idle;
        session.loading = true;
        let cwd = session.options.cwd.clone();
        let read = cx.background_spawn(async move {
            flux_claude::transcript::load(&transcript, cwd).map_err(|err| (transcript, err))
        });
        session._load = Some(cx.spawn(async move |this, cx| {
            let loaded = read.await;
            this.update(cx, |this, cx| this.loaded(loaded, cx)).ok();
        }));
        session
    }

    fn idle(cli: Cli, options: LaunchOptions) -> Self {
        Self {
            model: Session::new(options.cwd.clone()),
            cli,
            options,
            process: None,
            _pump: None,
            mcp: McpServer::new(crate::claude_tools::specs()),
            tool_calls: HashMap::new(),
            edit_hooks: HashMap::new(),
            loading: false,
            _load: None,
        }
    }

    /// The transcript is read: its conversation goes before whatever happened meanwhile (a message
    /// sent while it was read).
    fn loaded(
        &mut self,
        loaded: Result<Session, (PathBuf, std::io::Error)>,
        cx: &mut Context<Self>,
    ) {
        self.loading = false;
        self._load = None;
        match loaded {
            Ok(saved) => {
                let changes = self.model.prepend_history(saved);
                self.emit(changes, cx);
            }
            Err((path, err)) => {
                eprintln!("flux: can't read the Claude session {}: {err}", path.display());
                cx.emit(SessionEvent::Changed);
                cx.notify();
            }
        }
    }

    /// The title the history knows of a resumed session (the transcript's): shown at once, before
    /// the transcript is read; the CLI's later titles and a rename replace it.
    pub fn set_title_hint(&mut self, title: String) {
        if self.model.info.title.is_none() && !title.trim().is_empty() {
            self.model.info.title = Some(title);
        }
    }

    /// The transcript of a resumed session is still being read.
    pub fn is_loading(&self) -> bool {
        self.loading
    }

    /// The process runs (a resumed session has none until it is needed).
    pub fn is_started(&self) -> bool {
        self.process.is_some()
    }

    /// Starts the process of a session that has none (a resumed one in sight): the commands,
    /// the models and the limits arrive.
    pub fn ensure_started(&mut self, cx: &mut Context<Self>) {
        if self.process.is_none() {
            self.options.resume = self.model.info.session_id.clone();
            self.start(cx);
        }
    }

    /// The window ran a Flux tool Claude called ([`SessionEvent::ToolCall`]).
    pub fn answer_tool(&mut self, request: &str, output: ToolOutput, _cx: &mut Context<Self>) {
        let Some(id) = self.tool_calls.remove(request) else {
            return;
        };
        if let Some(process) = &self.process {
            process.respond(
                request,
                json!({ "mcp_response": McpServer::result(&id, &output) }),
            );
        }
    }

    /// The window looked at the file an edit changed ([`SessionEvent::EditHook`]): `problems` —
    /// what language servers now find wrong in it, for Claude; `None` — nothing to tell.
    pub fn answer_edit_hook(
        &mut self,
        request: &str,
        problems: Option<String>,
        _cx: &mut Context<Self>,
    ) {
        // Answered already (by the window or after the wait).
        let Some(mut response) = self.edit_hooks.remove(request) else {
            return;
        };
        if let Some(problems) = problems.filter(|problems| !problems.trim().is_empty()) {
            let context = match response["hookSpecificOutput"]["additionalContext"].as_str() {
                Some(note) => format!("{note}\n\n{problems}"),
                None => problems,
            };
            response["hookSpecificOutput"] = json!({
                "hookEventName": "PostToolUse",
                "additionalContext": context,
            });
        }
        if let Some(process) = &self.process {
            process.respond(request, response);
        }
    }

    /// These files leave the session's review (committed, rolled back to their text before Claude).
    pub fn forget_changed_files(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        let changes = self.model.forget_changed_files(paths);
        self.emit(changes, cx);
    }

    /// The conversation as the chat shows it.
    pub fn model(&self) -> &Session {
        &self.model
    }

    /// The tab's title: the CLI's title of the conversation, otherwise "New Session".
    pub fn title(&self) -> SharedString {
        self.model
            .info
            .title
            .clone()
            .map(SharedString::from)
            .unwrap_or_else(|| tr("New Session").into())
    }

    pub fn session_id(&self) -> Option<&str> {
        self.model.info.session_id.as_deref()
    }

    /// Sends the user's message; a session whose process exited starts again, resuming the
    /// conversation.
    pub fn send(&mut self, input: UserInput, cx: &mut Context<Self>) {
        if self.process.is_none() {
            self.options.resume = self.model.info.session_id.clone();
            self.start(cx);
        }
        let Some(process) = &self.process else {
            return;
        };
        let uuid = process.send_user(&input);
        let changes = self.model.push_user(Some(uuid), &input);
        self.emit(changes, cx);
    }

    /// Answers a question of the CLI (a permission, an edit, Claude's questions, the plan).
    pub fn answer(&mut self, request: &str, answer: Answer, cx: &mut Context<Self>) {
        let Some(pending) = self.model.pending(request).cloned() else {
            return;
        };
        if let Some(process) = &self.process {
            process.respond(request, Session::response(&pending, &answer));
        }
        let changes = self.model.resolve(request, &answer);
        self.emit(changes, cx);
    }

    /// Stops the running turn (Esc). A pending question is withdrawn by the CLI itself
    /// (`control_cancel_request`).
    pub fn interrupt(&mut self, cx: &mut Context<Self>) {
        self.request(HostRequest::Interrupt, cx);
    }

    /// Takes back a message queued while a turn runs.
    pub fn cancel_queued(&mut self, uuid: &str, cx: &mut Context<Self>) {
        self.request(HostRequest::CancelQueued(uuid.to_string()), cx);
    }

    /// `None` — the account's default model.
    pub fn set_model(&mut self, model: Option<String>, cx: &mut Context<Self>) {
        self.options.model = model.clone();
        self.request(HostRequest::SetModel(model), cx);
    }

    pub fn set_permission_mode(&mut self, mode: PermissionMode, cx: &mut Context<Self>) {
        self.options.permission_mode = Some(mode);
        self.request(HostRequest::SetPermissionMode(mode), cx);
    }

    /// `None` — the model's default effort.
    pub fn set_effort(&mut self, effort: Option<Effort>, cx: &mut Context<Self>) {
        self.options.effort = effort;
        self.request(HostRequest::SetEffort(effort), cx);
    }

    pub fn rename(&mut self, title: String, cx: &mut Context<Self>) {
        self.request(HostRequest::RenameSession(title), cx);
    }

    /// Stops a background command or agent.
    pub fn stop_task(&mut self, task_id: String, cx: &mut Context<Self>) {
        self.request(HostRequest::StopTask(task_id), cx);
    }

    /// Asks how full the context window is (the meter under the field).
    pub fn refresh_context(&mut self, cx: &mut Context<Self>) {
        let Some(process) = &self.process else {
            return;
        };
        let answer = process.request(&HostRequest::GetContextUsage);
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(response)) = answer.await {
                this.update(cx, |this, cx| {
                    let changes = this.model.set_context(&response);
                    this.emit(changes, cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /// Asks the subscription's limits (they also come after each request; this fills the status
    /// bar before the first one).
    fn refresh_usage(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            for attempt in 0..USAGE_ATTEMPTS {
                if attempt > 0 {
                    cx.background_executor().timer(USAGE_RETRY).await;
                }
                let Ok(Some(answer)) = this.update(cx, |this, _| {
                    this.process
                        .as_ref()
                        .map(|process| process.request(&HostRequest::GetUsage))
                }) else {
                    return;
                };
                let Ok(Ok(response)) = answer.await else {
                    return;
                };
                let filled = this
                    .update(cx, |this, cx| {
                        let changes = this.model.set_usage(&response);
                        let filled = !changes.is_empty();
                        this.emit(changes, cx);
                        filled
                    })
                    .unwrap_or(true);
                if filled {
                    return;
                }
            }
        })
        .detach();
    }

    /// A control request whose answer only matters as an error (the CLI then keeps its state).
    fn request(&mut self, request: HostRequest, cx: &mut Context<Self>) {
        let Some(process) = &self.process else {
            return;
        };
        let answer = process.request(&request);
        cx.spawn(async move |_, _| {
            if let Ok(Err(error)) = answer.await {
                eprintln!("flux: claude refused {request:?}: {error}");
            }
        })
        .detach();
    }

    /// Ends the process (the window closes, the session closes).
    pub fn shutdown(&mut self) {
        self.tool_calls.clear();
        self.edit_hooks.clear();
        if let Some(process) = self.process.take() {
            process.close();
            // The pump stops with it: the model learns about the end here (the chat stops showing
            // "working" after Open in Terminal).
            self.model.apply(&ProcessEvent::Exited {
                code: Some(0),
                stderr: Vec::new(),
            });
        }
        self._pump = None;
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        let (process, events) = match Process::spawn(&self.cli, &self.options) {
            Ok(started) => started,
            Err(err) => {
                let changes = self.model.apply(&ProcessEvent::Exited {
                    code: None,
                    stderr: vec![format!("{}: {err}", self.cli.path.display())],
                });
                self.emit(changes, cx);
                return;
            }
        };
        let initialized = process.request(&self.initialize_request(cx));
        self.process = Some(process);
        // The answer of `initialize` comes before the frames of the first turn; the frames are
        // read meanwhile by the pump.
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(response)) = initialized.await {
                this.update(cx, |this, cx| {
                    let changes = this.model.initialized(&response);
                    this.emit(changes, cx);
                    this.refresh_usage(cx);
                })
                .ok();
            }
        })
        .detach();
        self._pump = Some(
            cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let mut events = events;
                while let Some(event) = events.next().await {
                    if this.update(cx, |this, cx| this.handle(event, cx)).is_err() {
                        break;
                    }
                }
            }),
        );
    }

    /// The first request: the edit hook of the session model, and with Flux's tools on — the
    /// `flux` MCP server and the hook that allows its tools.
    fn initialize_request(&self, cx: &Context<Self>) -> HostRequest {
        let mut request = Session::initialize_request();
        if !crate::settings::claude(cx).flux_tools {
            return request;
        }
        if let HostRequest::Initialize(fields) = &mut request {
            fields["sdkMcpServers"] = json!([mcp::SERVER]);
            let hooks = fields["hooks"]["PreToolUse"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let mut hooks = hooks;
            hooks.push(json!({
                "matcher": format!("{}.*", mcp::tool_name("")),
                "hookCallbackIds": [TOOLS_HOOK],
            }));
            fields["hooks"]["PreToolUse"] = Value::Array(hooks);
        }
        request
    }

    fn handle(&mut self, event: ProcessEvent, cx: &mut Context<Self>) {
        if let ProcessEvent::Frame(frame) = &event
            && let Incoming::Request { id, request } = frame.as_ref()
        {
            self.answer_request(id, request, cx);
        }
        // The pump ends by itself: the channel closes after this event. The requests waiting for
        // the window go with the process.
        if matches!(event, ProcessEvent::Exited { .. }) {
            self.process = None;
            self.tool_calls.clear();
            self.edit_hooks.clear();
        }
        let changes = self.model.apply(&event);
        self.emit(changes, cx);
    }

    /// The CLI's requests the chat doesn't show are answered at once; permissions wait for the
    /// user, Flux's tools for the window.
    fn answer_request(&mut self, id: &str, request: &CliRequest, cx: &mut Context<Self>) {
        let Some(process) = &self.process else {
            return;
        };
        match request {
            CliRequest::CanUseTool(_) => {}
            CliRequest::HookCallback { callback_id, input } if callback_id == EDIT_HOOK => {
                let response = self.model.hook_response(input);
                let path = edited_path(input)
                    .filter(|_| crate::settings::claude(cx).report_problems);
                let Some(path) = path else {
                    return process.respond(id, response);
                };
                // The window first reads the file again (the CLI reports the edit only after the
                // hook), so that the language server checks Claude's text; then it answers.
                self.edit_hooks.insert(id.to_string(), response);
                cx.emit(SessionEvent::FilesChanged(vec![path.clone()]));
                cx.emit(SessionEvent::EditHook {
                    request: id.to_string(),
                    path,
                });
                let request = id.to_string();
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(EDIT_HOOK_WAIT).await;
                    this.update(cx, |this, cx| this.answer_edit_hook(&request, None, cx))
                        .ok();
                })
                .detach();
            }
            CliRequest::HookCallback { callback_id, .. } => {
                let response = if callback_id == TOOLS_HOOK {
                    json!({
                        "hookSpecificOutput": {
                            "hookEventName": "PreToolUse",
                            "permissionDecision": "allow",
                            "permissionDecisionReason": "A tool of Flux",
                        },
                    })
                } else {
                    json!({})
                };
                process.respond(id, response);
            }
            CliRequest::McpMessage { server, message } if server == mcp::SERVER => {
                match self.mcp.handle(message) {
                    McpReply::Respond(response) => {
                        process.respond(id, json!({ "mcp_response": response }))
                    }
                    McpReply::Call {
                        id: call,
                        name,
                        arguments,
                    } => {
                        self.tool_calls.insert(id.to_string(), call);
                        cx.emit(SessionEvent::ToolCall {
                            request: id.to_string(),
                            tool: name,
                            input: arguments,
                        });
                    }
                }
            }
            CliRequest::McpMessage { server, .. } => {
                process.respond_error(id, &format!("No MCP server {server}"))
            }
            CliRequest::Other { subtype, .. } => match subtype.as_str() {
                // An MCP server asks the user for input: Flux has no form for it yet.
                "elicitation" => process.respond(id, json!({ "action": "decline" })),
                "request_user_dialog" => process.respond(id, json!({ "behavior": "cancelled" })),
                _ => process.respond_error(id, &format!("Unsupported request: {subtype}")),
            },
        }
    }

    fn emit(&mut self, changes: Vec<Change>, cx: &mut Context<Self>) {
        if changes.is_empty() {
            return;
        }
        let mut redraw = false;
        let mut finished = false;
        for change in changes {
            let event = match change {
                Change::Entries
                | Change::Info
                | Change::Context
                | Change::Tasks
                | Change::Background
                | Change::Queue => {
                    redraw = true;
                    continue;
                }
                Change::Status => SessionEvent::Status,
                Change::PendingAdded(id) => SessionEvent::PendingAdded(id),
                Change::PendingRemoved(id) => SessionEvent::PendingRemoved(id),
                Change::TurnFinished { is_error } => {
                    finished = true;
                    SessionEvent::TurnFinished { is_error }
                }
                Change::Idle => SessionEvent::Idle,
                Change::Title => SessionEvent::Title,
                Change::Limits => SessionEvent::Limits,
                Change::FilesChanged(paths) => SessionEvent::FilesChanged(paths),
                Change::Exited => SessionEvent::Exited,
            };
            cx.emit(event);
        }
        if redraw {
            cx.emit(SessionEvent::Changed);
        }
        // The meter under the field follows each turn.
        if finished {
            self.refresh_context(cx);
        }
        cx.notify();
    }
}

/// The file an edit changed, from the input of its PostToolUse hook (Edit, MultiEdit, Write).
fn edited_path(input: &Value) -> Option<PathBuf> {
    if input["hook_event_name"] != "PostToolUse"
        || !matches!(
            input["tool_name"].as_str(),
            Some("Edit" | "MultiEdit" | "Write")
        )
    {
        return None;
    }
    input["tool_input"]["file_path"]
        .as_str()
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

impl Drop for ClaudeSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}
