//! One Claude Code conversation of the window: the `claude` process in host mode and the session
//! model built from its frames ([`flux_claude::Session`]). The chat reads the model and calls the
//! methods here; what changed comes out as [`SessionEvent`]s.
//!
//! The process starts with the session (the commands and the models come with `initialize`, the
//! subscription's limits with `get_usage`) and again with `--resume` when the user writes after it
//! exited. Every request of the CLI is answered here exactly once: permissions wait for the user
//! (the chat answers them through [`ClaudeSession::answer`]), the rest at once.

use std::time::Duration;

use futures::StreamExt;
use gpui::{AsyncApp, Context, EventEmitter, SharedString, Task, WeakEntity};
use serde_json::json;

use flux_claude::process::ProcessEvent;
use flux_claude::protocol::{CliRequest, HostRequest, Incoming};
use flux_claude::session::EDIT_HOOK;
use flux_claude::{
    Answer, Change, Cli, Effort, LaunchOptions, PermissionMode, Process, Session, UserInput,
};

use crate::i18n::tr;

/// How often `get_usage` is asked again while the CLI hasn't fetched the limits yet (right after
/// it starts it answers `rate_limits: null`).
const USAGE_ATTEMPTS: usize = 4;
const USAGE_RETRY: Duration = Duration::from_secs(3);

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
}

pub struct ClaudeSession {
    model: Session,
    cli: Cli,
    options: LaunchOptions,
    process: Option<Process>,
    _pump: Option<Task<()>>,
}

impl EventEmitter<SessionEvent> for ClaudeSession {}

impl ClaudeSession {
    pub fn new(cli: Cli, options: LaunchOptions, cx: &mut Context<Self>) -> Self {
        let mut session = Self {
            model: Session::new(options.cwd.clone()),
            cli,
            options,
            process: None,
            _pump: None,
        };
        session.start(cx);
        session
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
        let initialized = process.request(&Session::initialize_request());
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

    fn handle(&mut self, event: ProcessEvent, cx: &mut Context<Self>) {
        if let ProcessEvent::Frame(frame) = &event
            && let Incoming::Request { id, request } = frame.as_ref()
        {
            self.answer_request(id, request);
        }
        // The pump ends by itself: the channel closes after this event.
        if matches!(event, ProcessEvent::Exited { .. }) {
            self.process = None;
        }
        let changes = self.model.apply(&event);
        self.emit(changes, cx);
    }

    /// The CLI's requests the chat doesn't show are answered at once; permissions wait for the
    /// user.
    fn answer_request(&mut self, id: &str, request: &CliRequest) {
        let Some(process) = &self.process else {
            return;
        };
        match request {
            CliRequest::CanUseTool(_) => {}
            CliRequest::HookCallback { callback_id, input } => {
                let response = if callback_id == EDIT_HOOK {
                    self.model.hook_response(input)
                } else {
                    json!({})
                };
                process.respond(id, response);
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

impl Drop for ClaudeSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}
