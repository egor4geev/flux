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
//! Part 10.2: `!` commands ([`ClaudeSession::run_shell`]) run here, in the project's root with the
//! login shell's environment; when one ends and Claude is idle, its records go to the CLI as
//! context only (or as a message Claude answers — a setting).
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
use flux_claude::shell::{self, ShellCommand, ShellEvent};
use flux_claude::{
    Answer, Change, Cli, Effort, EntryId, LaunchOptions, PermissionMode, Process, Session,
    ShellState, Status, UserInput,
};

use crate::i18n::tr;

/// How often `get_usage` is asked again while the CLI hasn't fetched the limits yet (right after
/// it starts it answers `rate_limits: null`).
const USAGE_ATTEMPTS: usize = 4;
const USAGE_RETRY: Duration = Duration::from_secs(3);
/// How long an edit's hook waits for the window to say what the language server found: then the
/// CLI gets the answer without it (Claude waits for the hook).
const EDIT_HOOK_WAIT: Duration = Duration::from_secs(4);
/// While a `!` command runs, the chat is redrawn this often (its clock).
const SHELL_TICK: Duration = Duration::from_secs(1);
/// What a `!` command keeps of each stream for Claude, at most: past it the middle goes (Claude
/// gets [`shell::OUTPUT_LIMIT`] characters of it anyway).
const SHELL_KEEP: usize = 1 << 20;

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

/// Where an edited message goes back to (part 10.2): the conversation from the message `from` on
/// is dropped; the process continues the session at the entry `resume_at` of its transcript (the
/// message's parent), or starts a new session when the message opened the conversation.
#[derive(Debug, Clone, PartialEq)]
pub struct EditPoint {
    pub from: EntryId,
    pub resume_at: Option<String>,
}

/// A restart waiting for the old process to end: then the new one starts at `resume_at` with the
/// edited message.
struct Restart {
    resume_at: Option<String>,
    input: UserInput,
}

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
    /// `!` commands, running or ended and not handed to Claude yet (part 10.2).
    shells: HashMap<EntryId, ShellRun>,
    /// Ended `!` commands whose records wait for Claude to be idle, in the order they ended.
    held: Vec<EntryId>,
    /// Redraws the chat each second while a `!` command runs.
    shell_ticker: Option<Task<()>>,
    /// An edited message waits for the old process to end (part 10.2); its frames are ignored
    /// meanwhile.
    restart: Option<Restart>,
    /// What the message field starts with (part 10.3: a plugin's session with its context); the
    /// chat's field takes it when it is made.
    draft: Option<SessionDraft>,
    /// A redraw of the conversation waits for the next animation tick ([`Self::emit`]).
    redraw_pending: bool,
}

/// The start of a session's message field: text, and attachments as chips (part 10.3).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionDraft {
    pub prompt: Option<String>,
    pub attachments: Vec<Attachment>,
}

/// Something that goes along with a message (part 10.3): a chip in the field with its label, its
/// Markdown in the message ([`attachments_text`]) — a plugin's task, say.
#[derive(Clone, Debug, PartialEq)]
pub struct Attachment {
    pub label: String,
    pub markdown: String,
}

/// The attachments as Claude reads them, before the message: each in a tag named by its label.
pub fn attachments_text(attachments: &[Attachment]) -> String {
    attachments
        .iter()
        .map(|attachment| {
            format!(
                "<attachment name=\"{}\">\n{}\n</attachment>",
                attachment.label.replace('"', "'"),
                attachment.markdown.trim()
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// A message with its attachments in front, as it goes to Claude.
pub fn with_attachments(attachments: &[Attachment], message: &str) -> String {
    let attachments = attachments_text(attachments);
    match (attachments.is_empty(), message.trim().is_empty()) {
        (true, _) => message.to_string(),
        (false, true) => attachments,
        (false, false) => format!("{attachments}\n\n{message}"),
    }
}

/// A `!` command: its process and what it printed, for Claude.
struct ShellRun {
    command: String,
    /// `None` while it starts (the login shell's environment is read first) and when it couldn't.
    process: Option<ShellCommand>,
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
    /// The user stopped it.
    stopped: bool,
    ended: bool,
    /// Claude answers its output (Settings → Claude Code, ⌘↵ the other way); otherwise the output
    /// is context for the next message.
    answer: bool,
    _pump: Option<Task<()>>,
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
            shells: HashMap::new(),
            held: Vec::new(),
            shell_ticker: None,
            restart: None,
            draft: None,
            redraw_pending: false,
        }
    }

    /// What the chat's field starts with (part 10.3), set before the chat is made.
    pub fn set_draft(&mut self, draft: SessionDraft) {
        self.draft = Some(draft);
    }

    /// The field's start, once: the chat's field takes it.
    pub fn take_draft(&mut self) -> Option<SessionDraft> {
        self.draft.take()
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
                eprintln!(
                    "flux: can't read the Claude session {}: {err}",
                    path.display()
                );
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
        // The output of `!` commands that waited goes before the message: Claude reads it with it.
        self.deliver_held(false, cx);
        let Some(process) = &self.process else {
            return;
        };
        let uuid = process.send_user(&input);
        let changes = self.model.push_user(Some(uuid), &input);
        self.emit(changes, cx);
    }

    /// Where the message `id` can be edited from (part 10.2): its parent in the CLI's transcript.
    /// An error says why it can't — the message isn't in the record (it went in while Claude
    /// worked), the record can't be read.
    pub fn edit_point(
        &self,
        id: EntryId,
        cx: &mut Context<Self>,
    ) -> Task<Result<EditPoint, String>> {
        let uuid = self
            .model
            .entries
            .iter()
            .find_map(|entry| match &entry.kind {
                flux_claude::EntryKind::User(user) if entry.id == id => Some(user.uuid.clone()),
                _ => None,
            });
        let Some(uuid) = uuid else {
            return Task::ready(Err(tr("The message isn't in the conversation").to_string()));
        };
        // A session that never started has no record: the message opened it.
        let Some(session_id) = self.model.info.session_id.clone() else {
            return Task::ready(Ok(EditPoint {
                from: id,
                resume_at: None,
            }));
        };
        let missing = tr("The session's record doesn't have this message").to_string();
        let Some((uuid, path)) = uuid.zip(flux_claude::transcript::transcript_path(
            &self.options.cwd,
            &session_id,
        )) else {
            return Task::ready(Err(missing));
        };
        cx.background_spawn(async move {
            match flux_claude::transcript::parent_of(&path, &uuid) {
                Ok(Some(resume_at)) => Ok(EditPoint {
                    from: id,
                    resume_at,
                }),
                Ok(None) => Err(missing),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Err(missing),
                Err(err) => Err(err.to_string()),
            }
        })
    }

    /// Sends an edited message (part 10.2): the conversation from the old message on goes, and the
    /// process starts again on the session cut at the message's parent (`--resume-session-at`).
    /// A running turn is interrupted first, `!` commands after that point are dropped.
    pub fn send_edited(&mut self, point: EditPoint, input: UserInput, cx: &mut Context<Self>) {
        self.held.retain(|id| *id < point.from);
        let dropped: Vec<EntryId> = self
            .shells
            .keys()
            .filter(|id| **id >= point.from)
            .copied()
            .collect();
        for id in dropped {
            if let Some(run) = self.shells.remove(&id)
                && let Some(process) = &run.process
            {
                process.stop();
            }
        }
        let working = self.model.is_working();
        let changes = self.model.truncate_from(point.from);
        self.emit(changes, cx);
        self.restart = Some(Restart {
            resume_at: point.resume_at,
            input,
        });
        if self.process.is_none() {
            return self.restart_now(cx);
        }
        if working {
            // The turn stops before stdin closes: a closed stdin lets it run to its end.
            self.request(HostRequest::Interrupt, cx);
        }
        if let Some(process) = &self.process {
            process.close();
        }
    }

    /// The old process is gone: the new one starts where the edited message goes, and gets it.
    fn restart_now(&mut self, cx: &mut Context<Self>) {
        let Some(restart) = self.restart.take() else {
            return;
        };
        self.options.resume = restart
            .resume_at
            .as_ref()
            .and_then(|_| self.model.info.session_id.clone());
        self.options.resume_at = restart.resume_at;
        self.options.session_id = None;
        self.start(cx);
        // Later starts (after the process exits) continue the session whole.
        self.options.resume_at = None;
        self.send(restart.input, cx);
    }

    /// Flux put files back as they were before Claude's edits (part 10.2): open documents take
    /// them from disk; files back to their text before Claude leave the session's changelist.
    pub fn files_restored(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let unchanged: Vec<PathBuf> = paths
            .iter()
            .filter(|path| {
                self.model.changed_file(path).is_some_and(|file| {
                    let disk = std::fs::read_to_string(path).ok();
                    disk.as_deref() == file.original.as_deref()
                })
            })
            .cloned()
            .collect();
        if !unchanged.is_empty() {
            self.forget_changed_files(&unchanged, cx);
        }
        cx.emit(SessionEvent::FilesChanged(paths));
        cx.notify();
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

    /// Runs a `!` command (part 10.2): in the project's root, through the user's shell with the
    /// login shell's environment (a terminal's `PATH`, also when Flux started from the Dock). Its
    /// entry shows the output as it comes; when it ends, Claude gets it — and answers it when
    /// `answer`.
    pub fn run_shell(&mut self, command: String, answer: bool, cx: &mut Context<Self>) {
        let command = command.trim().to_string();
        if command.is_empty() {
            return;
        }
        let (id, changes) = self.model.start_shell(command.clone());
        self.emit(changes, cx);
        let cwd = self.options.cwd.clone();
        let line = command.clone();
        // The first command of the process waits for the login shell to tell its environment.
        let started = cx.background_spawn(async move {
            shell::spawn(
                &line,
                &cwd,
                &shell::user_shell(),
                Some(flux_plugin::login_env()),
            )
        });
        let pump = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            match started.await {
                Ok((process, mut events)) => {
                    let started = this.update(cx, |this, _| match this.shells.get_mut(&id) {
                        Some(run) => {
                            // Stopped before it started.
                            if run.stopped {
                                process.stop();
                            }
                            run.process = Some(process);
                            true
                        }
                        None => false,
                    });
                    if !matches!(started, Ok(true)) {
                        return;
                    }
                    while let Some(event) = events.next().await {
                        if this
                            .update(cx, |this, cx| this.shell_event(id, event, cx))
                            .is_err()
                        {
                            break;
                        }
                    }
                }
                Err(err) => {
                    this.update(cx, |this, cx| this.shell_failed(id, err.to_string(), cx))
                        .ok();
                }
            }
        });
        self.shells.insert(
            id,
            ShellRun {
                command,
                process: None,
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
                stopped: false,
                ended: false,
                answer,
                _pump: Some(pump),
            },
        );
        self.update_shell_ticker(cx);
    }

    /// Stops a running `!` command and what it started; what it printed so far still goes to
    /// Claude.
    pub fn stop_shell(&mut self, id: EntryId, _cx: &mut Context<Self>) {
        let Some(run) = self.shells.get_mut(&id).filter(|run| !run.ended) else {
            return;
        };
        run.stopped = true;
        if let Some(process) = &run.process {
            process.stop();
        }
    }

    /// The `!` command that runs, the latest one (the Stop button of the field).
    pub fn running_shell(&self) -> Option<EntryId> {
        self.shells
            .iter()
            .filter(|(_, run)| !run.ended)
            .map(|(id, _)| *id)
            .max()
    }

    fn shell_event(&mut self, id: EntryId, event: ShellEvent, cx: &mut Context<Self>) {
        let Some(run) = self.shells.get_mut(&id) else {
            return;
        };
        match event {
            ShellEvent::Stdout(text) => {
                keep(&mut run.stdout, &text);
                let changes = self.model.shell_output(id, &text);
                self.emit(changes, cx);
            }
            ShellEvent::Stderr(text) => {
                keep(&mut run.stderr, &text);
                let changes = self.model.shell_output(id, &text);
                self.emit(changes, cx);
            }
            ShellEvent::Exited { code, duration } => {
                run.ended = true;
                run.exit_code = code;
                run.process = None;
                let state = if run.stopped {
                    ShellState::Stopped
                } else {
                    ShellState::Done
                };
                let changes = self.model.shell_ended(id, state, code, Some(duration));
                self.emit(changes, cx);
                self.held.push(id);
                self.deliver_when_idle(cx);
            }
        }
    }

    /// The command couldn't start (no shell, no such folder): the reason as its output, for
    /// Claude too.
    fn shell_failed(&mut self, id: EntryId, error: String, cx: &mut Context<Self>) {
        let Some(run) = self.shells.get_mut(&id) else {
            return;
        };
        run.ended = true;
        run.stderr = error.clone();
        let mut changes = self.model.shell_output(id, &error);
        changes.extend(self.model.shell_ended(id, ShellState::Failed, None, None));
        self.emit(changes, cx);
        self.held.push(id);
        self.deliver_when_idle(cx);
    }

    /// The records of the ended `!` commands go to Claude now, unless Claude works (then when it
    /// is idle: a message of context in the middle of a turn would join the turn).
    fn deliver_when_idle(&mut self, cx: &mut Context<Self>) {
        let busy = matches!(
            self.model.status,
            Status::Working { .. } | Status::WaitingForUser | Status::Starting
        );
        // Without a process (a resumed session, an exited one) the records start it.
        if self.held.is_empty() || busy && self.process.is_some() {
            return;
        }
        // One turn after all the records, when any of the commands asked for an answer.
        let respond = self
            .held
            .iter()
            .any(|id| self.shells.get(id).is_some_and(|run| run.answer));
        self.deliver_held(respond, cx);
    }

    /// Hands the held records to the CLI: each command — its input, then its output; `respond` —
    /// Claude answers the last one (a turn), otherwise all are context only.
    fn deliver_held(&mut self, respond: bool, cx: &mut Context<Self>) {
        if self.held.is_empty() {
            return;
        }
        // Taken first: starting the process reports changes, which come back here.
        let held = std::mem::take(&mut self.held);
        if self.process.is_none() {
            self.options.resume = self.model.info.session_id.clone();
            self.start(cx);
        }
        if self.process.is_none() {
            self.held = held;
            return;
        }
        let last = held.len() - 1;
        for (index, id) in held.into_iter().enumerate() {
            let Some(run) = self.shells.remove(&id) else {
                continue;
            };
            let Some(process) = &self.process else {
                return;
            };
            let query = respond && index == last;
            let mut stderr = run.stderr;
            if run.stopped {
                if !stderr.is_empty() && !stderr.ends_with('\n') {
                    stderr.push('\n');
                }
                stderr.push_str("(stopped by the user)");
            }
            process.send_context(&shell::input_record(&run.command), false);
            process.send_context(
                &shell::output_record(&run.stdout, &stderr, run.exit_code),
                query,
            );
            let changes = self.model.shell_delivered(id, if query { 1 } else { 2 });
            self.emit(changes, cx);
        }
    }

    /// Redraws the chat each second while a `!` command runs (its clock), and stops when none
    /// does.
    fn update_shell_ticker(&mut self, cx: &mut Context<Self>) {
        if self.shell_ticker.is_some() {
            return;
        }
        self.shell_ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(SHELL_TICK).await;
                let running = this.update(cx, |this, cx| {
                    let running = this.running_shell().is_some();
                    if running {
                        cx.emit(SessionEvent::Changed);
                        cx.notify();
                    } else {
                        this.shell_ticker = None;
                    }
                    running
                });
                if !matches!(running, Ok(true)) {
                    break;
                }
            }
        }));
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
        for run in self.shells.values() {
            if let Some(process) = &run.process {
                process.stop();
            }
        }
        self.shells.clear();
        self.held.clear();
        self.shell_ticker = None;
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
        // An edited message waits for this process to end: what it still says is gone with the
        // part of the conversation the edit dropped.
        if self.restart.is_some() {
            if matches!(event, ProcessEvent::Exited { .. }) {
                self.process = None;
                self.tool_calls.clear();
                self.edit_hooks.clear();
                // Outside the old process's pump, which this event comes from: the new process
                // replaces it.
                cx.spawn(async move |this, cx| {
                    this.update(cx, |this, cx| this.restart_now(cx)).ok();
                })
                .detach();
            }
            return;
        }
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
                let path =
                    edited_path(input).filter(|_| crate::settings::claude(cx).report_problems);
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
        // Nothing but the conversation and its lines changed (streamed text, a tool's output).
        let only_redraw = changes.iter().all(|change| {
            matches!(
                change,
                Change::Entries
                    | Change::Info
                    | Change::Context
                    | Change::Tasks
                    | Change::Background
                    | Change::Queue
            )
        });
        if only_redraw {
            self.redraw_on_tick(cx);
            return;
        }
        let mut redraw = false;
        let mut finished = false;
        // The status moved (the process is ready, a turn ended): held `!` records may go.
        let mut status = false;
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
                Change::Status => {
                    status = true;
                    SessionEvent::Status
                }
                Change::PendingAdded(id) => SessionEvent::PendingAdded(id),
                Change::PendingRemoved(id) => SessionEvent::PendingRemoved(id),
                Change::TurnFinished { is_error } => {
                    finished = true;
                    SessionEvent::TurnFinished { is_error }
                }
                Change::Idle => {
                    status = true;
                    SessionEvent::Idle
                }
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
        // Claude is idle again: the `!` commands that ended meanwhile go to it.
        if status && !self.held.is_empty() {
            self.deliver_when_idle(cx);
        }
    }

    /// Redraws the conversation at the next animation tick — in the same frame as the pulse of a
    /// working session — rather than at once: streamed text comes 30–100 times a second, and each
    /// redraw is a redraw of the whole window (2026-10-16: 17–30 % CPU while streaming, 6–9 % on
    /// the tick).
    fn redraw_on_tick(&mut self, cx: &mut Context<Self>) {
        if self.redraw_pending {
            return;
        }
        self.redraw_pending = true;
        let wait = gpui::until_next_animation_tick();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            this.update(cx, |this, cx| {
                this.redraw_pending = false;
                cx.emit(SessionEvent::Changed);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

/// Adds output of a `!` command to what Claude will get: a lone CR rewrites the line; past
/// [`SHELL_KEEP`] the middle goes.
fn keep(kept: &mut String, text: &str) {
    shell::append(kept, text);
    if kept.len() > SHELL_KEEP {
        *kept = shell::cut(kept, SHELL_KEEP / 2);
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
