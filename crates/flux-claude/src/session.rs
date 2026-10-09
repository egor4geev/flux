//! The conversation as the chat shows it, built from the CLI's frames without any I/O: entries
//! (the user's messages, Claude's text and thinking, tool calls with their results and subagents,
//! notices, turn ends), the status, the questions waiting for the user, the task list, background
//! tasks, limits and context. [`Session::apply`] takes a frame and says what changed.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::edits::{self, EditProposal};
use crate::process::ProcessEvent;
use crate::protocol::{
    Block, BlockStart, CliRequest, HostRequest, Image, Incoming, Lifecycle, Message, SessionState,
    StreamKind, System, TaskEvent, ToolPermission, TurnResult,
};
use crate::types::{
    BackgroundTask, ContextUsage, Effort, ImageAttachment, ModelInfo, PermissionMode, RateLimits,
    SlashCommand, UserInput,
};

/// Stable while the session lives: the chat keeps per-entry state (expanded, copied) by it.
pub type EntryId = u64;

#[derive(Debug, Clone)]
pub struct Session {
    pub info: SessionInfo,
    /// The conversation, oldest first. A subagent's messages are inside its Agent call
    /// ([`ToolEntry::children`]).
    pub entries: Vec<Entry>,
    pub status: Status,
    /// Questions for the user (permissions, edits, Claude's questions, the plan), oldest first.
    pub pending: Vec<Pending>,
    /// Claude's task list (TaskCreate / TaskUpdate, or the legacy TodoWrite).
    pub tasks: Vec<TaskItem>,
    pub background: Vec<BackgroundTask>,
    pub limits: Option<RateLimits>,
    pub context: Option<ContextUsage>,
    /// Spent by this process so far.
    pub cost_usd: f64,
    pub commands: Vec<SlashCommand>,
    pub models: Vec<ModelInfo>,
    /// Files Claude changed in this session with their text before the first change (the review
    /// of part 9.2).
    pub changed_files: Vec<ChangedFile>,
    next_id: EntryId,
    /// The blocks of the message being streamed, by their index.
    streaming: HashMap<usize, EntryId>,
    /// The input JSON of tool calls being streamed, so far.
    partial_inputs: HashMap<String, String>,
    /// Background and subagent tasks → their tool call (`task_updated` names only the task).
    task_tools: HashMap<String, String>,
    /// Edits the user changed before allowing them → what Claude is told after the edit ran
    /// ([`Session::hook_response`]).
    modified_edits: HashMap<String, String>,
    /// What this process has spent: `result.total_cost_usd` counts from the process's start.
    process_cost: f64,
    /// `session_state_changed` arrives (the CLI was started with the variable that enables it):
    /// then only it makes the session idle.
    has_state_events: bool,
}

/// The hook Flux registers in `initialize`: after an edit ran, the CLI asks it whether Claude
/// should learn anything (the user changed the edit before allowing it).
pub const EDIT_HOOK: &str = "flux-edit";

/// A user's version of an edit up to this size goes to Claude whole; a longer one — only the note
/// that the file differs from the proposal.
const MODIFIED_EDIT_TEXT_LIMIT: usize = 6_000;

/// What the session is.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SessionInfo {
    /// Known after the first turn's `init` (or given: `--resume`, `--session-id`).
    pub session_id: Option<String>,
    /// Made up by the CLI after the first exchange, or set by the user.
    pub title: Option<String>,
    /// The resolved model id: "claude-opus-5-5".
    pub model: Option<String>,
    pub permission_mode: PermissionMode,
    pub effort: Option<Effort>,
    pub version: Option<String>,
    pub cwd: PathBuf,
    pub tools: Vec<String>,
    /// Commands that only make sense in the terminal CLI.
    pub terminal_commands: Vec<String>,
    pub mcp_servers: Vec<(String, String)>,
    /// The account's email (from `initialize`).
    pub account: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    /// The process is starting.
    Starting,
    /// Nothing runs: the user's turn.
    Idle,
    Working {
        since: Instant,
        activity: Activity,
    },
    /// A question waits for the user (a permission, an edit, Claude's question, the plan).
    WaitingForUser,
    /// The process is gone; a new message starts it again (resuming the conversation).
    Exited {
        code: Option<i32>,
    },
}

impl Status {
    pub fn is_working(&self) -> bool {
        matches!(self, Status::Working { .. } | Status::WaitingForUser)
    }
}

/// What a working session does right now: the line under the conversation.
#[derive(Debug, Clone, PartialEq)]
pub enum Activity {
    Requesting,
    Thinking {
        tokens: u64,
    },
    Responding,
    /// Running a tool: its display name ("Bash", "Edit").
    Tool(String),
    Compacting,
    Retrying {
        attempt: u32,
        max: u32,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub id: EntryId,
    pub kind: EntryKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EntryKind {
    User(UserEntry),
    /// Claude's text (Markdown).
    Text {
        text: String,
        streaming: bool,
    },
    /// Claude's thinking: summarized text (may be empty), the token estimate, how long it took.
    Thinking {
        text: String,
        tokens: u64,
        streaming: bool,
        duration: Option<Duration>,
        started: Instant,
    },
    Tool(Box<ToolEntry>),
    Notice(Notice),
    /// The end of a turn.
    TurnEnd(TurnSummary),
}

#[derive(Debug, Clone, PartialEq)]
pub struct UserEntry {
    /// The uuid the message was sent with.
    pub uuid: Option<String>,
    pub text: String,
    pub images: Vec<ImageAttachment>,
    /// Waits for the running turn to end.
    pub queued: bool,
    /// Taken back before it started.
    pub cancelled: bool,
}

/// A tool call and what came of it.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolEntry {
    pub tool_use_id: String,
    /// The tool's name: "Edit", "Bash", "mcp__flux__open_in_editor".
    pub name: String,
    pub input: Value,
    pub call: ToolCall,
    pub state: ToolState,
    pub result: Option<ToolResult>,
    /// A subagent's conversation (Agent calls).
    pub children: Vec<Entry>,
    /// A subagent's or a background command's progress.
    pub task: Option<TaskProgress>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolState {
    /// The input is still being generated.
    Streaming,
    /// Waits for the user's permission.
    Waiting,
    Running,
    Done,
    Failed,
    /// The user (or a rule, the mode) refused it.
    Denied,
    /// The turn was interrupted before it finished.
    Interrupted,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    /// What the model got back.
    pub text: String,
    pub is_error: bool,
    pub images: Vec<Image>,
    /// The tool's own result object: an edit's `structuredPatch` and `originalFile`, a command's
    /// `stdout` and `stderr`, a subagent's totals.
    pub structured: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct TaskProgress {
    pub description: Option<String>,
    pub last_tool: Option<String>,
    pub tokens: Option<u64>,
    pub tool_uses: Option<u64>,
    /// running, completed, failed, killed…
    pub status: Option<String>,
    pub summary: Option<String>,
    pub background: bool,
}

/// A tool call's input, read for display.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolCall {
    Read {
        path: PathBuf,
        offset: Option<u64>,
        limit: Option<u64>,
    },
    Edit {
        path: PathBuf,
        old: String,
        new: String,
        replace_all: bool,
    },
    Write {
        path: PathBuf,
        content: String,
    },
    NotebookEdit {
        path: PathBuf,
    },
    Bash {
        command: String,
        description: Option<String>,
        background: bool,
    },
    Grep {
        pattern: String,
        path: Option<String>,
    },
    Glob {
        pattern: String,
    },
    WebFetch {
        url: String,
    },
    WebSearch {
        query: String,
    },
    /// A subagent ("Agent", listed as "Task").
    Agent {
        description: String,
        subagent_type: Option<String>,
        prompt: String,
        background: bool,
    },
    TaskCreate {
        subject: String,
    },
    TaskUpdate {
        task_id: String,
        status: Option<String>,
    },
    TodoWrite {
        todos: Vec<TaskItem>,
    },
    AskUserQuestion {
        questions: Vec<Question>,
    },
    ExitPlanMode {
        plan: String,
    },
    EnterPlanMode,
    Skill {
        name: String,
    },
    /// `mcp__<server>__<tool>`.
    Mcp {
        server: String,
        tool: String,
    },
    Other,
}

impl ToolCall {
    pub fn parse(name: &str, input: &Value) -> ToolCall {
        let text = |key: &str| input[key].as_str().unwrap_or("").to_string();
        let path = |key: &str| PathBuf::from(input[key].as_str().unwrap_or(""));
        match name {
            "Read" => ToolCall::Read {
                path: path("file_path"),
                offset: input["offset"].as_u64(),
                limit: input["limit"].as_u64(),
            },
            "Edit" => ToolCall::Edit {
                path: path("file_path"),
                old: text("old_string"),
                new: text("new_string"),
                replace_all: input["replace_all"].as_bool().unwrap_or(false),
            },
            "MultiEdit" => ToolCall::Edit {
                path: path("file_path"),
                old: String::new(),
                new: String::new(),
                replace_all: false,
            },
            "Write" => ToolCall::Write {
                path: path("file_path"),
                content: text("content"),
            },
            "NotebookEdit" => ToolCall::NotebookEdit {
                path: path("notebook_path"),
            },
            "Bash" => ToolCall::Bash {
                command: text("command"),
                description: input["description"].as_str().map(str::to_string),
                background: input["run_in_background"].as_bool().unwrap_or(false),
            },
            "Grep" => ToolCall::Grep {
                pattern: text("pattern"),
                path: input["path"].as_str().map(str::to_string),
            },
            "Glob" => ToolCall::Glob {
                pattern: text("pattern"),
            },
            "WebFetch" => ToolCall::WebFetch { url: text("url") },
            "WebSearch" => ToolCall::WebSearch {
                query: text("query"),
            },
            "Agent" | "Task" => ToolCall::Agent {
                description: text("description"),
                subagent_type: input["subagent_type"].as_str().map(str::to_string),
                prompt: text("prompt"),
                background: input["run_in_background"].as_bool().unwrap_or(false),
            },
            "TaskCreate" => ToolCall::TaskCreate {
                subject: text("subject"),
            },
            "TaskUpdate" => ToolCall::TaskUpdate {
                task_id: text("taskId"),
                status: input["status"].as_str().map(str::to_string),
            },
            "TodoWrite" => ToolCall::TodoWrite {
                todos: input["todos"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .enumerate()
                    .map(|(index, todo)| TaskItem {
                        id: (index + 1).to_string(),
                        subject: todo["content"].as_str().unwrap_or("").to_string(),
                        active_form: todo["activeForm"].as_str().map(str::to_string),
                        status: TaskStatus::from_wire(todo["status"].as_str().unwrap_or("")),
                    })
                    .collect(),
            },
            "AskUserQuestion" => ToolCall::AskUserQuestion {
                questions: Question::list(input),
            },
            "ExitPlanMode" => ToolCall::ExitPlanMode { plan: text("plan") },
            "EnterPlanMode" => ToolCall::EnterPlanMode,
            "Skill" => ToolCall::Skill {
                name: text("skill"),
            },
            _ => match name
                .strip_prefix("mcp__")
                .and_then(|rest| rest.split_once("__"))
            {
                Some((server, tool)) => ToolCall::Mcp {
                    server: server.to_string(),
                    tool: tool.to_string(),
                },
                None => ToolCall::Other,
            },
        }
    }

    /// The file an edit changes.
    pub fn edited_path(&self) -> Option<&PathBuf> {
        match self {
            ToolCall::Edit { path, .. }
            | ToolCall::Write { path, .. }
            | ToolCall::NotebookEdit { path } => Some(path),
            _ => None,
        }
    }
}

/// One of Claude's questions (AskUserQuestion).
#[derive(Debug, Clone, PartialEq)]
pub struct Question {
    pub question: String,
    /// A short chip label (≤12 characters).
    pub header: String,
    pub options: Vec<QuestionOption>,
    pub multi_select: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
    /// What to show while the option is focused (code, a mockup).
    pub preview: Option<String>,
}

impl Question {
    pub fn list(input: &Value) -> Vec<Question> {
        input["questions"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|question| Question {
                question: question["question"].as_str().unwrap_or("").to_string(),
                header: question["header"].as_str().unwrap_or("").to_string(),
                options: question["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|option| QuestionOption {
                        label: option["label"].as_str().unwrap_or("").to_string(),
                        description: option["description"].as_str().unwrap_or("").to_string(),
                        preview: option["preview"].as_str().map(str::to_string),
                    })
                    .collect(),
                multi_select: question["multiSelect"].as_bool().unwrap_or(false),
            })
            .collect()
    }
}

/// An item of Claude's task list.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskItem {
    pub id: String,
    pub subject: String,
    /// "Running the tests": shown while it is in progress.
    pub active_form: Option<String>,
    pub status: TaskStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
}

impl TaskStatus {
    pub fn from_wire(status: &str) -> Self {
        match status {
            "in_progress" => TaskStatus::InProgress,
            "completed" => TaskStatus::Completed,
            _ => TaskStatus::Pending,
        }
    }
}

/// Something that happened in the conversation besides the messages.
#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    /// The user stopped the turn.
    Interrupted,
    Compacted {
        trigger: String,
        pre_tokens: Option<u64>,
        post_tokens: Option<u64>,
    },
    Retrying {
        attempt: u32,
        max: u32,
        error: Option<String>,
    },
    /// A request failed: `kind` — "rate_limit", "authentication_failed", "billing_error",
    /// "overloaded"…
    Error { kind: String, text: String },
    /// A call refused without asking (the mode, a rule).
    Denied { tool: String, message: String },
    /// A message of the CLI ("notification", "informational").
    Info { level: String, text: String },
    /// The reply of a local slash command (`/usage`, `/context`, `/compact`): its text and the
    /// frame with the command's data (`usage_report`, `context_usage`).
    LocalCommand { text: String, raw: Value },
    /// The process ended on its own: its exit code and last lines of stderr.
    Exited {
        code: Option<i32>,
        stderr: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct TurnSummary {
    pub duration: Duration,
    /// What this turn cost (in the API's prices; with a subscription — what it would have cost).
    pub cost_usd: f64,
    pub is_error: bool,
    /// "completed", "aborted_streaming", "max_turns"…
    pub reason: Option<String>,
    pub errors: Vec<String>,
}

/// A question waiting for the user.
#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    /// The CLI's request id: the answer goes back with it.
    pub id: String,
    pub tool_use_id: String,
    pub tool: String,
    pub display_name: String,
    /// A ready label: the file name, the command's description, the URL.
    pub description: Option<String>,
    pub input: Value,
    pub kind: PendingKind,
    /// "Always allow" choices, sent back as they are.
    pub suggestions: Vec<Value>,
    pub blocked_path: Option<String>,
    pub reason: Option<String>,
    pub default_to_no: bool,
    pub suppress_always_allow: bool,
    /// A subagent asks.
    pub from_agent: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PendingKind {
    /// Allow or deny a tool call (a command, a fetch, an MCP tool, a path outside the project).
    Tool,
    /// An edit: the diff of the file.
    Edit(EditProposal),
    /// Claude's questions with options.
    Questions(Vec<Question>),
    /// The plan of plan mode, for approval.
    Plan { plan: String, path: Option<PathBuf> },
}

/// The user's answer to a pending question.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// `remember` — the chosen "always allow" suggestions (from [`Pending::suggestions`]).
    Allow {
        remember: Vec<Value>,
    },
    /// `message` goes to Claude; `interrupt` also stops the turn.
    Deny {
        message: String,
        interrupt: bool,
    },
    /// The user's version of an edit (the proposal, possibly trimmed or edited in the diff).
    Edit {
        text: String,
        remember: Vec<Value>,
    },
    /// Claude's questions: the question's text → the chosen labels (or the user's own text).
    Answers(Vec<(String, String)>),
    ApprovePlan {
        accept_edits: bool,
    },
    /// The plan isn't approved: Claude keeps planning with this feedback.
    KeepPlanning(String),
}

/// A file Claude changed in this session.
#[derive(Debug, Clone, PartialEq)]
pub struct ChangedFile {
    pub path: PathBuf,
    /// The text before Claude's first change in the session; `None` — Claude created it.
    pub original: Option<String>,
}

/// What [`Session::apply`] changed: the chat redraws, the window notifies, opens a diff.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Entries,
    Status,
    PendingAdded(String),
    PendingRemoved(String),
    /// A turn ended (a `result`).
    TurnFinished {
        is_error: bool,
    },
    /// Nothing runs any more (after the turn and whatever background work followed it).
    Idle,
    Title,
    /// The model, the mode, effort, the commands, the tools.
    Info,
    Limits,
    Context,
    Tasks,
    Background,
    /// A queued message started, completed or was taken back.
    Queue,
    /// Tools changed these files on disk.
    FilesChanged(Vec<PathBuf>),
    Exited,
}

impl Session {
    pub fn new(cwd: PathBuf) -> Self {
        Session {
            info: SessionInfo {
                cwd,
                ..SessionInfo::default()
            },
            entries: Vec::new(),
            status: Status::Starting,
            pending: Vec::new(),
            tasks: Vec::new(),
            background: Vec::new(),
            limits: None,
            context: None,
            cost_usd: 0.,
            commands: Vec::new(),
            models: Vec::new(),
            changed_files: Vec::new(),
            next_id: 1,
            streaming: HashMap::new(),
            partial_inputs: HashMap::new(),
            task_tools: HashMap::new(),
            modified_edits: HashMap::new(),
            process_cost: 0.,
            has_state_events: false,
        }
    }

    /// The first request to the CLI: registers the edit hook (the user's changes to an edit reach
    /// Claude through it).
    pub fn initialize_request() -> HostRequest {
        HostRequest::Initialize(json!({
            "hooks": {
                "PostToolUse": [{
                    "matcher": "Edit|MultiEdit|Write",
                    "hookCallbackIds": [EDIT_HOOK],
                }],
            },
        }))
    }

    pub fn is_working(&self) -> bool {
        self.status.is_working()
    }

    pub fn pending(&self, id: &str) -> Option<&Pending> {
        self.pending.iter().find(|pending| pending.id == id)
    }

    /// The tool call by its id, anywhere (subagents included).
    pub fn tool(&self, tool_use_id: &str) -> Option<&ToolEntry> {
        fn find<'a>(entries: &'a [Entry], id: &str) -> Option<&'a ToolEntry> {
            entries.iter().find_map(|entry| match &entry.kind {
                EntryKind::Tool(tool) if tool.tool_use_id == id => Some(tool.as_ref()),
                EntryKind::Tool(tool) => find(&tool.children, id),
                _ => None,
            })
        }
        find(&self.entries, tool_use_id)
    }

    /// The user's message, shown at once (the CLI doesn't echo it). Sent while a turn runs, it is
    /// queued: it stays at the end of the conversation, below what the running turn still adds,
    /// until its own turn starts.
    pub fn push_user(&mut self, uuid: Option<String>, input: &UserInput) -> Vec<Change> {
        let queued = self.is_working() && input.priority != Some(crate::types::Priority::Now);
        let entry = Entry {
            id: self.next_id,
            kind: EntryKind::User(UserEntry {
                uuid,
                text: input.text.clone(),
                images: input.images.clone(),
                queued,
                cancelled: false,
            }),
        };
        self.next_id += 1;
        // After the other queued messages: they go in the order they were sent.
        self.entries.push(entry);
        if !self.is_working() {
            self.set_working(Activity::Requesting);
        }
        vec![Change::Entries, Change::Status, Change::Queue]
    }

    /// The answer of `initialize`: the commands, the models, the account.
    pub fn initialized(&mut self, response: &Value) -> Vec<Change> {
        self.commands = crate::protocol::commands(&response["commands"]);
        self.models = crate::protocol::models(&response["models"]);
        self.info.account = response["account"]["email"].as_str().map(str::to_string);
        if let Some(mode) = response["current_permission_mode"]
            .as_str()
            .and_then(PermissionMode::from_wire)
        {
            self.info.permission_mode = mode;
        }
        if self.status == Status::Starting {
            self.status = Status::Idle;
        }
        vec![Change::Info, Change::Status]
    }

    /// The answer of `list_models`.
    pub fn set_models(&mut self, response: &Value) -> Vec<Change> {
        let models = crate::protocol::models(&response["models"]);
        if models.is_empty() {
            return Vec::new();
        }
        self.models = models;
        vec![Change::Info]
    }

    /// The answer of `get_context_usage`.
    pub fn set_context(&mut self, response: &Value) -> Vec<Change> {
        match (
            response["totalTokens"].as_u64(),
            response["maxTokens"].as_u64(),
        ) {
            (Some(used), Some(max)) => {
                self.context = Some(ContextUsage { used, max });
                vec![Change::Context]
            }
            _ => Vec::new(),
        }
    }

    /// The answer of `get_usage`: the subscription's limits; nothing while the CLI hasn't fetched
    /// them yet (ask again a little later).
    pub fn set_usage(&mut self, response: &Value) -> Vec<Change> {
        match RateLimits::from_usage(response) {
            Some(limits) => {
                self.limits = Some(limits);
                vec![Change::Limits]
            }
            None => Vec::new(),
        }
    }

    /// Records the user's answer: the question goes, the tool row shows what was decided. An edit
    /// the user changed is remembered: Claude learns the real text after it ran
    /// ([`Session::hook_response`]).
    pub fn resolve(&mut self, id: &str, answer: &Answer) -> Vec<Change> {
        let Some(index) = self.pending.iter().position(|pending| pending.id == id) else {
            return Vec::new();
        };
        let pending = self.pending.remove(index);
        if let (Answer::Edit { text, .. }, PendingKind::Edit(proposal)) = (answer, &pending.kind)
            && proposal.proposed.as_deref().ok() != Some(text.as_str())
        {
            self.modified_edits.insert(
                pending.tool_use_id.clone(),
                modified_edit_note(&proposal.path, text),
            );
        }
        let state = match answer {
            Answer::Deny { .. } | Answer::KeepPlanning(_) => ToolState::Denied,
            _ => ToolState::Running,
        };
        if let Some(tool) = self.tool_mut(&pending.tool_use_id) {
            tool.state = state;
        }
        if self.pending.is_empty() && self.status == Status::WaitingForUser {
            self.set_working(Activity::Tool(pending.display_name.clone()));
        }
        vec![
            Change::PendingRemoved(pending.id),
            Change::Entries,
            Change::Status,
        ]
    }

    /// The answer to a `hook_callback` of [`EDIT_HOOK`]: after an edit the user changed, Claude is
    /// told what the file really holds (the CLI tells it nothing: it applies the changed input as
    /// its own). `{}` — nothing to add.
    pub fn hook_response(&mut self, input: &Value) -> Value {
        let tool_use_id = input["tool_use_id"].as_str().unwrap_or("");
        match self.modified_edits.remove(tool_use_id) {
            Some(note) if input["hook_event_name"] == "PostToolUse" => json!({
                "hookSpecificOutput": {
                    "hookEventName": "PostToolUse",
                    "additionalContext": note,
                },
            }),
            _ => json!({}),
        }
    }

    /// The `response` of the `can_use_tool` answer.
    pub fn response(pending: &Pending, answer: &Answer) -> Value {
        let allow = |input: Value, remember: &[Value]| {
            let mut response = json!({ "behavior": "allow", "updatedInput": input });
            if !remember.is_empty() {
                response["updatedPermissions"] = Value::from(remember.to_vec());
            }
            response
        };
        match answer {
            Answer::Allow { remember } => allow(pending.input.clone(), remember),
            Answer::Deny { message, interrupt } => {
                let mut response = json!({ "behavior": "deny", "message": message });
                if *interrupt {
                    response["interrupt"] = Value::Bool(true);
                }
                response
            }
            Answer::Edit { text, remember } => {
                let (original, proposed) = match &pending.kind {
                    PendingKind::Edit(proposal) => (
                        proposal.original.as_deref(),
                        proposal.proposed.as_deref().ok(),
                    ),
                    _ => (None, None),
                };
                let input =
                    edits::updated_input(&pending.tool, &pending.input, original, proposed, text);
                allow(input, remember)
            }
            Answer::Answers(answers) => {
                let mut input = pending.input.clone();
                // Several labels of one multi-select question are joined by ", ".
                let mut map = serde_json::Map::new();
                for (question, answer) in answers {
                    let joined = match map.get(question).and_then(Value::as_str) {
                        Some(previous) => format!("{previous}, {answer}"),
                        None => answer.clone(),
                    };
                    map.insert(question.clone(), Value::from(joined));
                }
                input["answers"] = Value::Object(map);
                allow(input, &[])
            }
            Answer::ApprovePlan { accept_edits } => {
                let remember = if *accept_edits {
                    vec![
                        json!({ "type": "setMode", "mode": "acceptEdits", "destination": "session" }),
                    ]
                } else {
                    Vec::new()
                };
                allow(pending.input.clone(), &remember)
            }
            Answer::KeepPlanning(feedback) => json!({ "behavior": "deny", "message": feedback }),
        }
    }

    /// Applies an event of the process.
    pub fn apply(&mut self, event: &ProcessEvent) -> Vec<Change> {
        match event {
            ProcessEvent::Frame(frame) => self.apply_frame(frame),
            ProcessEvent::Exited { code, stderr } => {
                self.finish_streaming();
                let mut changes: Vec<Change> = self
                    .pending
                    .drain(..)
                    .map(|pending| Change::PendingRemoved(pending.id))
                    .collect();
                self.mark_running(ToolState::Interrupted);
                for entry in &mut self.entries {
                    if let EntryKind::User(user) = &mut entry.kind
                        && user.queued
                    {
                        user.queued = false;
                        user.cancelled = true;
                    }
                }
                self.background.clear();
                self.partial_inputs.clear();
                self.process_cost = 0.;
                // A process that ends while the user works with it (not closed by Flux) says why.
                if *code != Some(0) || self.entries.is_empty() {
                    self.push(EntryKind::Notice(Notice::Exited {
                        code: *code,
                        stderr: stderr.clone(),
                    }));
                }
                self.status = Status::Exited { code: *code };
                changes.extend([
                    Change::Entries,
                    Change::Background,
                    Change::Status,
                    Change::Exited,
                ]);
                changes
            }
        }
    }

    fn apply_frame(&mut self, frame: &Incoming) -> Vec<Change> {
        match frame {
            Incoming::System(system) => self.apply_system(system),
            Incoming::Stream(event) => {
                // Subagents don't stream; their frames come whole.
                if event.parent_tool_use_id.is_some() {
                    return Vec::new();
                }
                self.apply_stream(&event.kind)
            }
            Incoming::Assistant(message) => self.apply_assistant(message),
            Incoming::User(message) => self.apply_user(message),
            Incoming::Result(result) => self.apply_result(result),
            Incoming::RateLimit(limits) => {
                self.limits = Some(limits.clone());
                vec![Change::Limits]
            }
            Incoming::Lifecycle { message, state } => self.apply_lifecycle(message, *state),
            Incoming::Request { id, request } => match request {
                CliRequest::CanUseTool(permission) => self.add_pending(id, permission),
                _ => Vec::new(),
            },
            Incoming::Cancel { id } => {
                let Some(index) = self.pending.iter().position(|pending| &pending.id == id) else {
                    return Vec::new();
                };
                let pending = self.pending.remove(index);
                if let Some(tool) = self.tool_mut(&pending.tool_use_id) {
                    tool.state = ToolState::Interrupted;
                }
                vec![Change::PendingRemoved(pending.id), Change::Entries]
            }
            Incoming::Response { .. } | Incoming::KeepAlive | Incoming::Unknown(_) => Vec::new(),
        }
    }

    fn apply_system(&mut self, system: &System) -> Vec<Change> {
        match system {
            System::Init(init) => {
                self.info.session_id = Some(init.session_id.clone()).filter(|id| !id.is_empty());
                self.info.model = Some(init.model.clone()).filter(|model| !model.is_empty());
                if let Some(mode) = init.permission_mode {
                    self.info.permission_mode = mode;
                }
                self.info.effort = init.effort;
                self.info.version = Some(init.version.clone()).filter(|v| !v.is_empty());
                self.info.tools = init.tools.clone();
                self.info.terminal_commands = init.terminal_commands.clone();
                self.info.mcp_servers = init.mcp_servers.clone();
                vec![Change::Info]
            }
            System::Status {
                status,
                permission_mode,
            } => {
                let mut changes = Vec::new();
                if let Some(mode) = permission_mode
                    && *mode != self.info.permission_mode
                {
                    self.info.permission_mode = *mode;
                    changes.push(Change::Info);
                }
                match status.as_deref() {
                    Some("requesting") if self.pending.is_empty() => {
                        self.set_working(Activity::Requesting);
                        changes.push(Change::Status);
                    }
                    Some("compacting") => {
                        self.set_working(Activity::Compacting);
                        changes.push(Change::Status);
                    }
                    _ => {}
                }
                changes
            }
            System::State(state) => {
                self.has_state_events = true;
                match state {
                    SessionState::Idle => {
                        self.finish_streaming();
                        self.status = Status::Idle;
                        vec![Change::Status, Change::Idle]
                    }
                    SessionState::Running => {
                        if !matches!(self.status, Status::Working { .. }) {
                            self.set_working(Activity::Requesting);
                        }
                        vec![Change::Status]
                    }
                    SessionState::RequiresAction => {
                        self.status = Status::WaitingForUser;
                        vec![Change::Status]
                    }
                }
            }
            System::ThinkingTokens(tokens) => {
                let id = self
                    .streaming
                    .values()
                    .copied()
                    .filter(|id| {
                        self.entry(*id)
                            .is_some_and(|entry| matches!(entry.kind, EntryKind::Thinking { .. }))
                    })
                    .max();
                if let Some(entry) = id.and_then(|id| self.entry_mut(id))
                    && let EntryKind::Thinking { tokens: shown, .. } = &mut entry.kind
                {
                    *shown = *tokens;
                }
                if matches!(self.status, Status::Working { .. }) {
                    self.set_activity(Activity::Thinking { tokens: *tokens });
                }
                vec![Change::Entries, Change::Status]
            }
            System::Compacted {
                trigger,
                pre_tokens,
                post_tokens,
            } => {
                self.push(EntryKind::Notice(Notice::Compacted {
                    trigger: trigger.clone(),
                    pre_tokens: *pre_tokens,
                    post_tokens: *post_tokens,
                }));
                vec![Change::Entries]
            }
            System::Task(event) => self.apply_task(event),
            System::BackgroundTasks(tasks) => {
                self.background = tasks.clone();
                vec![Change::Background]
            }
            System::PermissionDenied {
                tool_use_id,
                tool_name,
                message,
            } => {
                if let Some(tool) = self.tool_mut(tool_use_id) {
                    tool.state = ToolState::Denied;
                }
                self.push(EntryKind::Notice(Notice::Denied {
                    tool: tool_name.clone(),
                    message: message.clone(),
                }));
                vec![Change::Entries]
            }
            System::Title(title) => {
                self.info.title = Some(title.clone()).filter(|title| !title.is_empty());
                vec![Change::Title]
            }
            System::Commands(commands) => {
                self.commands = commands.clone();
                vec![Change::Info]
            }
            System::ApiRetry {
                attempt,
                max_retries,
                error,
                ..
            } => {
                self.set_working(Activity::Retrying {
                    attempt: *attempt,
                    max: *max_retries,
                });
                self.push(EntryKind::Notice(Notice::Retrying {
                    attempt: *attempt,
                    max: *max_retries,
                    error: error.clone(),
                }));
                vec![Change::Entries, Change::Status]
            }
            System::Notice { level, text } => {
                if text.is_empty() {
                    return Vec::new();
                }
                self.push(EntryKind::Notice(Notice::Info {
                    level: level.clone(),
                    text: text.clone(),
                }));
                vec![Change::Entries]
            }
            System::Other { subtype, raw } => match subtype.as_str() {
                // The model refused and another one answered (or none could).
                "model_refusal_fallback" | "model_refusal_no_fallback" => {
                    let text = raw["content"].as_str().unwrap_or("").to_string();
                    self.push(EntryKind::Notice(Notice::Info {
                        level: "warning".into(),
                        text,
                    }));
                    vec![Change::Entries]
                }
                _ => Vec::new(),
            },
        }
    }

    fn apply_stream(&mut self, kind: &StreamKind) -> Vec<Change> {
        match kind {
            StreamKind::MessageStart { .. } => {
                // A new message of the turn: blocks are numbered from 0 again. Blocks of the
                // previous message the CLI never finished (no frame) stop streaming.
                self.finish_streaming_blocks();
                Vec::new()
            }
            StreamKind::BlockStart { index, block } => {
                let entry = match block {
                    BlockStart::Text => EntryKind::Text {
                        text: String::new(),
                        streaming: true,
                    },
                    BlockStart::Thinking => EntryKind::Thinking {
                        text: String::new(),
                        tokens: 0,
                        streaming: true,
                        duration: None,
                        started: Instant::now(),
                    },
                    BlockStart::ToolUse { id, name } => {
                        self.set_working(Activity::Tool(name.clone()));
                        self.partial_inputs.insert(id.clone(), String::new());
                        EntryKind::Tool(Box::new(ToolEntry {
                            tool_use_id: id.clone(),
                            name: name.clone(),
                            input: Value::Null,
                            call: ToolCall::parse(name, &Value::Null),
                            state: ToolState::Streaming,
                            result: None,
                            children: Vec::new(),
                            task: None,
                        }))
                    }
                    BlockStart::Other => return Vec::new(),
                };
                match &entry {
                    EntryKind::Text { .. } => self.set_activity(Activity::Responding),
                    EntryKind::Thinking { .. } => {
                        self.set_activity(Activity::Thinking { tokens: 0 })
                    }
                    _ => {}
                }
                let id = self.push(entry);
                self.streaming.insert(*index, id);
                vec![Change::Entries, Change::Status]
            }
            StreamKind::TextDelta { index, text } | StreamKind::ThinkingDelta { index, text } => {
                let Some(id) = self.streaming.get(index).copied() else {
                    return Vec::new();
                };
                match self.entry_mut(id).map(|entry| &mut entry.kind) {
                    Some(EntryKind::Text { text: shown, .. })
                    | Some(EntryKind::Thinking { text: shown, .. }) => shown.push_str(text),
                    _ => return Vec::new(),
                }
                vec![Change::Entries]
            }
            StreamKind::InputDelta { index, json } => {
                let Some(id) = self.streaming.get(index).copied() else {
                    return Vec::new();
                };
                let Some(Entry {
                    kind: EntryKind::Tool(tool),
                    ..
                }) = self.entry_mut(id)
                else {
                    return Vec::new();
                };
                if tool.state != ToolState::Streaming {
                    return Vec::new();
                }
                let tool_use_id = tool.tool_use_id.clone();
                let name = tool.name.clone();
                let partial = self.partial_inputs.entry(tool_use_id).or_default();
                partial.push_str(json);
                // What is known so far: the command, the path appear while Claude writes them.
                let Some(input) = parse_partial_json(partial) else {
                    return Vec::new();
                };
                if let Some(Entry {
                    kind: EntryKind::Tool(tool),
                    ..
                }) = self.entry_mut(id)
                {
                    tool.call = ToolCall::parse(&name, &input);
                    tool.input = input;
                }
                vec![Change::Entries]
            }
            StreamKind::BlockStop { .. }
            | StreamKind::MessageDelta { .. }
            | StreamKind::MessageStop
            | StreamKind::Other => Vec::new(),
        }
    }

    fn apply_assistant(&mut self, message: &Message) -> Vec<Change> {
        if message.synthetic {
            let text = message
                .content
                .iter()
                .filter_map(|block| match block {
                    Block::Text(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            self.push(EntryKind::Notice(Notice::LocalCommand {
                text,
                raw: message.raw.clone(),
            }));
            return vec![Change::Entries];
        }
        if let Some(kind) = &message.error {
            let text = message
                .content
                .iter()
                .find_map(|block| match block {
                    Block::Text(text) => Some(text.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            // The block streamed so far is the error's text: it goes, the notice stays.
            self.drop_streamed_text();
            self.push(EntryKind::Notice(Notice::Error {
                kind: kind.clone(),
                text,
            }));
            return vec![Change::Entries];
        }
        let parent = message.parent_tool_use_id.clone();
        let mut changes = vec![Change::Entries];
        for block in &message.content {
            match block {
                Block::Text(text) => self.commit_streamed(
                    parent.as_deref(),
                    |kind| match kind {
                        EntryKind::Text {
                            text: shown,
                            streaming,
                        } => {
                            *shown = text.clone();
                            *streaming = false;
                            true
                        }
                        _ => false,
                    },
                    || EntryKind::Text {
                        text: text.clone(),
                        streaming: false,
                    },
                ),
                Block::Thinking(text) => self.commit_thinking(parent.as_deref(), text),
                Block::RedactedThinking => self.commit_thinking(parent.as_deref(), ""),
                Block::ToolUse { id, name, input } => {
                    let call = ToolCall::parse(name, input);
                    self.partial_inputs.remove(id);
                    if let Some(tool) = self.tool_mut(id) {
                        tool.input = input.clone();
                        tool.call = call;
                        if tool.state == ToolState::Streaming {
                            tool.state = ToolState::Running;
                        }
                    } else {
                        let entry = EntryKind::Tool(Box::new(ToolEntry {
                            tool_use_id: id.clone(),
                            name: name.clone(),
                            input: input.clone(),
                            call,
                            state: ToolState::Running,
                            result: None,
                            children: Vec::new(),
                            task: None,
                        }));
                        self.push_in(parent.as_deref(), entry);
                    }
                    if parent.is_none() {
                        self.set_activity(Activity::Tool(name.clone()));
                        changes.push(Change::Status);
                    }
                    if self.track_tasks_from_call(name, input) {
                        changes.push(Change::Tasks);
                    }
                }
                _ => {}
            }
        }
        if message.aborted {
            self.finish_streaming();
        }
        changes
    }

    fn apply_user(&mut self, message: &Message) -> Vec<Change> {
        if message.replay || message.is_meta {
            return Vec::new();
        }
        let mut changes = Vec::new();
        for block in &message.content {
            match block {
                Block::ToolResult {
                    tool_use_id,
                    text,
                    images,
                    is_error,
                } => {
                    let structured = message.tool_use_result.clone();
                    let not_run = message
                        .non_executed
                        .iter()
                        .find(|(id, _)| id == tool_use_id)
                        .map(|(_, kind)| kind.as_str());
                    let mut edited = None;
                    let mut created_task = None;
                    if let Some(tool) = self.tool_mut(tool_use_id) {
                        tool.state = match (tool.state, not_run, *is_error) {
                            (ToolState::Denied | ToolState::Interrupted, _, _) => tool.state,
                            (_, Some("user-rejected"), _) => ToolState::Interrupted,
                            (_, Some(_), _) => ToolState::Denied,
                            (_, None, true) => ToolState::Failed,
                            (_, None, false) => ToolState::Done,
                        };
                        if tool.state == ToolState::Done {
                            edited = tool.call.edited_path().cloned();
                            if let ToolCall::TaskCreate { subject } = &tool.call {
                                created_task = Some((subject.clone(), tool.input.clone()));
                            }
                        }
                        tool.result = Some(ToolResult {
                            text: text.clone(),
                            is_error: *is_error,
                            images: images.clone(),
                            structured: structured.clone(),
                        });
                    }
                    if let Some((subject, input)) = created_task {
                        let id = structured
                            .as_ref()
                            .and_then(|result| result["task"]["id"].as_str())
                            .map(str::to_string)
                            .unwrap_or_else(|| (self.tasks.len() + 1).to_string());
                        if !self.tasks.iter().any(|task| task.id == id) {
                            self.tasks.push(TaskItem {
                                id,
                                subject,
                                active_form: input["activeForm"].as_str().map(str::to_string),
                                status: TaskStatus::Pending,
                            });
                        }
                        changes.push(Change::Tasks);
                    }
                    if let Some(path) = edited {
                        // The file's text before the first change of the session, for the review.
                        let original =
                            structured
                                .as_ref()
                                .and_then(|result| match &result["originalFile"] {
                                    Value::String(text) => Some(Some(text.clone())),
                                    Value::Null if result["type"] == "create" => Some(None),
                                    _ => None,
                                });
                        if let Some(original) = original
                            && !self.changed_files.iter().any(|file| file.path == path)
                        {
                            self.changed_files.push(ChangedFile {
                                path: path.clone(),
                                original,
                            });
                        }
                        changes.push(Change::FilesChanged(vec![path]));
                    }
                    changes.push(Change::Entries);
                }
                Block::Text(text) if text.starts_with("[Request interrupted by user") => {
                    self.finish_streaming();
                    self.mark_running(ToolState::Interrupted);
                    self.push(EntryKind::Notice(Notice::Interrupted));
                    changes.push(Change::Entries);
                }
                _ => {}
            }
        }
        changes
    }

    fn apply_result(&mut self, result: &TurnResult) -> Vec<Change> {
        self.finish_streaming();
        // `total_cost_usd` counts from the process's start.
        let cost = (result.total_cost_usd - self.process_cost).max(0.);
        self.process_cost = self.process_cost.max(result.total_cost_usd);
        self.cost_usd += cost;
        if let (Some(used), Some(max)) = (result.context_tokens, result.context_window)
            && used > 0
        {
            self.context = Some(ContextUsage { used, max });
        }
        // A local command (`/cost`) makes a result of its own: no line for it.
        if result.local_command.is_none() {
            self.push(EntryKind::TurnEnd(TurnSummary {
                duration: Duration::from_millis(result.duration_ms),
                cost_usd: cost,
                is_error: result.is_error,
                reason: result.terminal_reason.clone(),
                errors: result.errors.clone(),
            }));
        }
        let mut changes = vec![
            Change::Entries,
            Change::Context,
            Change::TurnFinished {
                is_error: result.is_error,
            },
        ];
        // Without state events the result is the end of the work (unless more is queued).
        let queued = self.entries.iter().any(
            |entry| matches!(&entry.kind, EntryKind::User(user) if user.queued && !user.cancelled),
        );
        if !self.has_state_events && self.pending.is_empty() && !queued {
            self.status = Status::Idle;
            changes.extend([Change::Status, Change::Idle]);
        }
        changes
    }

    fn apply_lifecycle(&mut self, uuid: &str, state: Lifecycle) -> Vec<Change> {
        let Some(index) = self.entries.iter().rposition(|entry| {
            matches!(&entry.kind, EntryKind::User(user) if user.uuid.as_deref() == Some(uuid))
        }) else {
            return Vec::new();
        };
        let started_queued = matches!(
            (&self.entries[index].kind, state),
            (EntryKind::User(user), Lifecycle::Started) if user.queued
        );
        if let EntryKind::User(user) = &mut self.entries[index].kind {
            match state {
                Lifecycle::Queued => user.queued = true,
                Lifecycle::Started | Lifecycle::Completed => user.queued = false,
                // A message that already ran ends "cancelled" too when its turn is interrupted:
                // only a queued one was taken back.
                Lifecycle::Cancelled => {
                    if user.queued {
                        user.cancelled = true;
                    }
                    user.queued = false;
                }
            }
        }
        // Its turn starts: the message moves below what the previous turn added after it.
        if started_queued {
            let entry = self.entries.remove(index);
            let at = self.queued_tail_start();
            self.entries.insert(at, entry);
        }
        vec![Change::Entries, Change::Queue]
    }

    fn apply_task(&mut self, event: &TaskEvent) -> Vec<Change> {
        if let Some(tool_use_id) = &event.tool_use_id {
            self.task_tools
                .insert(event.task_id.clone(), tool_use_id.clone());
        }
        let Some(tool_use_id) = self.task_tools.get(&event.task_id).cloned() else {
            return Vec::new();
        };
        let Some(tool) = self.tool_mut(&tool_use_id) else {
            return Vec::new();
        };
        let task = tool.task.get_or_insert_with(TaskProgress::default);
        if event.description.is_some() {
            task.description = event.description.clone();
        }
        if event.last_tool.is_some() {
            task.last_tool = event.last_tool.clone();
        }
        if event.tokens.is_some() {
            task.tokens = event.tokens;
        }
        if event.tool_uses.is_some() {
            task.tool_uses = event.tool_uses;
        }
        if event.status.is_some() {
            task.status = event.status.clone();
        }
        if event.summary.is_some() {
            task.summary = event.summary.clone();
        }
        task.background |= event.background;
        vec![Change::Entries]
    }

    fn add_pending(&mut self, id: &str, permission: &ToolPermission) -> Vec<Change> {
        let kind = match permission.tool_name.as_str() {
            tool if edits::is_edit_tool(tool) => match edits::proposal(tool, &permission.input) {
                Some(proposal) => PendingKind::Edit(proposal),
                None => PendingKind::Tool,
            },
            "AskUserQuestion" => PendingKind::Questions(Question::list(&permission.input)),
            "ExitPlanMode" => PendingKind::Plan {
                plan: permission.input["plan"].as_str().unwrap_or("").to_string(),
                path: permission.input["planFilePath"].as_str().map(PathBuf::from),
            },
            _ => PendingKind::Tool,
        };
        if let Some(tool) = self.tool_mut(&permission.tool_use_id) {
            tool.state = ToolState::Waiting;
        }
        self.pending.push(Pending {
            id: id.to_string(),
            tool_use_id: permission.tool_use_id.clone(),
            tool: permission.tool_name.clone(),
            display_name: permission.display_name.clone(),
            description: permission.description.clone(),
            input: permission.input.clone(),
            kind,
            suggestions: permission.suggestions.clone(),
            blocked_path: permission.blocked_path.clone(),
            reason: permission.reason.clone(),
            default_to_no: permission.default_to_no,
            suppress_always_allow: permission.suppress_always_allow,
            from_agent: permission.agent_id.is_some(),
        });
        self.status = Status::WaitingForUser;
        vec![
            Change::PendingAdded(id.to_string()),
            Change::Entries,
            Change::Status,
        ]
    }

    /// TaskUpdate and TodoWrite change the task list as soon as Claude calls them.
    fn track_tasks_from_call(&mut self, name: &str, input: &Value) -> bool {
        match name {
            "TaskUpdate" => {
                let id = input["taskId"].as_str().unwrap_or("");
                match input["status"].as_str() {
                    Some("deleted") => self.tasks.retain(|task| task.id != id),
                    Some(status) => {
                        if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
                            task.status = TaskStatus::from_wire(status);
                        }
                    }
                    None => {}
                }
                if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
                    if let Some(subject) = input["subject"].as_str() {
                        task.subject = subject.to_string();
                    }
                    if let Some(active) = input["activeForm"].as_str() {
                        task.active_form = Some(active.to_string());
                    }
                }
                true
            }
            "TodoWrite" => {
                if let ToolCall::TodoWrite { todos } = ToolCall::parse(name, input) {
                    self.tasks = todos;
                }
                true
            }
            _ => false,
        }
    }

    // --- Helpers ---

    fn push(&mut self, kind: EntryKind) -> EntryId {
        self.push_in(None, kind)
    }

    /// Adds an entry to the main conversation (above the queued messages at its end) or to the
    /// subagent of `parent` (the main conversation when that call isn't known).
    fn push_in(&mut self, parent: Option<&str>, kind: EntryKind) -> EntryId {
        let id = self.next_id;
        self.next_id += 1;
        let entry = Entry { id, kind };
        match parent.and_then(|parent| self.tool_mut(parent)) {
            Some(tool) => tool.children.push(entry),
            None => {
                let at = self.queued_tail_start();
                self.entries.insert(at, entry);
            }
        }
        id
    }

    /// Where the queued messages at the end of the conversation begin.
    fn queued_tail_start(&self) -> usize {
        let queued = self
            .entries
            .iter()
            .rev()
            .take_while(|entry| {
                matches!(&entry.kind, EntryKind::User(user) if user.queued && !user.cancelled)
            })
            .count();
        self.entries.len() - queued
    }

    /// Finishes a streamed block with its final content (`commit` says whether the entry was the
    /// right kind), or adds the block when it wasn't streamed (subagents, older CLIs).
    fn commit_streamed(
        &mut self,
        parent: Option<&str>,
        commit: impl FnOnce(&mut EntryKind) -> bool,
        make: impl FnOnce() -> EntryKind,
    ) {
        if parent.is_none() {
            let streamed = self
                .streaming
                .values()
                .copied()
                .filter(|id| {
                    self.entry(*id).is_some_and(|entry| match &entry.kind {
                        EntryKind::Text { streaming, .. }
                        | EntryKind::Thinking { streaming, .. } => *streaming,
                        _ => false,
                    })
                })
                .min();
            if let Some(id) = streamed
                && let Some(entry) = self.entry_mut(id)
                && commit(&mut entry.kind)
            {
                return;
            }
        }
        self.push_in(parent, make());
    }

    fn commit_thinking(&mut self, parent: Option<&str>, text: &str) {
        self.commit_streamed(
            parent,
            |kind| match kind {
                EntryKind::Thinking {
                    text: shown,
                    streaming,
                    duration,
                    started,
                    ..
                } => {
                    if !text.is_empty() {
                        *shown = text.to_string();
                    }
                    *streaming = false;
                    *duration = Some(started.elapsed());
                    true
                }
                _ => false,
            },
            || EntryKind::Thinking {
                text: text.to_string(),
                tokens: 0,
                streaming: false,
                duration: None,
                started: Instant::now(),
            },
        );
    }

    /// The streamed text block of a request that failed: the error notice replaces it.
    fn drop_streamed_text(&mut self) {
        let ids: Vec<EntryId> = self.streaming.values().copied().collect();
        self.entries.retain(|entry| {
            !(ids.contains(&entry.id) && matches!(entry.kind, EntryKind::Text { .. }))
        });
        self.streaming.retain(|_, id| !ids.contains(id));
    }

    /// Blocks still marked streaming stop (a new message started, the turn ended).
    fn finish_streaming_blocks(&mut self) {
        for id in self.streaming.drain().map(|(_, id)| id).collect::<Vec<_>>() {
            if let Some(entry) = self.entry_mut(id) {
                match &mut entry.kind {
                    EntryKind::Text { streaming, .. } => *streaming = false,
                    EntryKind::Thinking {
                        streaming,
                        duration,
                        started,
                        ..
                    } => {
                        if *streaming {
                            *duration = Some(started.elapsed());
                        }
                        *streaming = false;
                    }
                    _ => {}
                }
            }
        }
    }

    /// The turn is over: streamed blocks stop, a tool call whose input never finished is cut off.
    fn finish_streaming(&mut self) {
        let ids: Vec<EntryId> = self.streaming.values().copied().collect();
        self.finish_streaming_blocks();
        for id in ids {
            if let Some(Entry {
                kind: EntryKind::Tool(tool),
                ..
            }) = self.entry_mut(id)
                && tool.state == ToolState::Streaming
            {
                tool.state = ToolState::Interrupted;
            }
        }
        self.partial_inputs.clear();
    }

    /// Tools that were still running end with `state`.
    fn mark_running(&mut self, state: ToolState) {
        fn mark(entries: &mut [Entry], state: ToolState) {
            for entry in entries {
                if let EntryKind::Tool(tool) = &mut entry.kind {
                    // A background command or agent goes on after the turn.
                    let background = tool.task.as_ref().is_some_and(|task| task.background);
                    if !background
                        && matches!(
                            tool.state,
                            ToolState::Streaming | ToolState::Waiting | ToolState::Running
                        )
                    {
                        tool.state = state;
                    }
                    mark(&mut tool.children, state);
                }
            }
        }
        mark(&mut self.entries, state);
    }

    fn set_working(&mut self, activity: Activity) {
        match &mut self.status {
            Status::Working {
                activity: shown, ..
            } => *shown = activity,
            _ => {
                self.status = Status::Working {
                    since: Instant::now(),
                    activity,
                }
            }
        }
    }

    fn set_activity(&mut self, activity: Activity) {
        if let Status::Working {
            activity: shown, ..
        } = &mut self.status
        {
            *shown = activity;
        }
    }

    fn entry(&self, id: EntryId) -> Option<&Entry> {
        fn find(entries: &[Entry], id: EntryId) -> Option<&Entry> {
            entries.iter().find_map(|entry| {
                if entry.id == id {
                    return Some(entry);
                }
                match &entry.kind {
                    EntryKind::Tool(tool) => find(&tool.children, id),
                    _ => None,
                }
            })
        }
        find(&self.entries, id)
    }

    fn entry_mut(&mut self, id: EntryId) -> Option<&mut Entry> {
        fn find(entries: &mut [Entry], id: EntryId) -> Option<&mut Entry> {
            for entry in entries {
                if entry.id == id {
                    return Some(entry);
                }
                if let EntryKind::Tool(tool) = &mut entry.kind
                    && let Some(found) = find(&mut tool.children, id)
                {
                    return Some(found);
                }
            }
            None
        }
        find(&mut self.entries, id)
    }

    fn tool_mut(&mut self, tool_use_id: &str) -> Option<&mut ToolEntry> {
        fn find<'a>(entries: &'a mut [Entry], id: &str) -> Option<&'a mut ToolEntry> {
            for entry in entries {
                if let EntryKind::Tool(tool) = &mut entry.kind {
                    if tool.tool_use_id == id {
                        return Some(tool.as_mut());
                    }
                    if let Some(found) = find(&mut tool.children, id) {
                        return Some(found);
                    }
                }
            }
            None
        }
        find(&mut self.entries, tool_use_id)
    }
}

/// What Claude is told after an edit the user changed: the file as it is now (or, for a long
/// one, that it should read it again).
fn modified_edit_note(path: &std::path::Path, text: &str) -> String {
    let path = path.display();
    if text.len() <= MODIFIED_EDIT_TEXT_LIMIT {
        format!(
            "The user changed your edit of {path} before it was applied, so the file does NOT \
             contain your proposed text. This is the file as it is now:\n\n```\n{text}\n```"
        )
    } else {
        format!(
            "The user changed your edit of {path} before it was applied, so the file does NOT \
             contain your proposed text. Read the file again before relying on its content."
        )
    }
}

/// The input of a tool call while Claude still writes it: the unfinished JSON closed as well as
/// it can be (an open string, open objects), dropping the last member while it doesn't parse.
fn parse_partial_json(text: &str) -> Option<Value> {
    let mut candidate = text.trim_end().to_string();
    for _ in 0..4 {
        if candidate.is_empty() {
            return None;
        }
        if let Some(value) = close_and_parse(&candidate) {
            return Some(value);
        }
        // Drop the last, unfinished member.
        let cut = last_separator(&candidate)?;
        candidate.truncate(cut);
    }
    None
}

fn close_and_parse(text: &str) -> Option<Value> {
    let mut closers = Vec::new();
    let (mut in_string, mut escaped) = (false, false);
    for c in text.chars() {
        if in_string {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => closers.push('}'),
            '[' => closers.push(']'),
            '}' | ']' => {
                closers.pop();
            }
            _ => {}
        }
    }
    let mut closed = text.to_string();
    if in_string {
        if escaped {
            closed.pop();
        }
        closed.push('"');
    } else if closed.ends_with(':') {
        closed.push_str("null");
    } else if closed.ends_with(',') {
        closed.pop();
    }
    closed.extend(closers.iter().rev());
    serde_json::from_str(&closed).ok()
}

/// The last `,` outside strings.
fn last_separator(text: &str) -> Option<usize> {
    let (mut in_string, mut escaped) = (false, false);
    let mut last = None;
    for (index, c) in text.char_indices() {
        if in_string {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            ',' => last = Some(index),
            _ => {}
        }
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unfinished_tool_input_shows_what_is_known() {
        let parse = |text| parse_partial_json(text);
        assert_eq!(
            parse(r#"{"command": "cargo te"#),
            Some(json!({ "command": "cargo te" }))
        );
        assert_eq!(
            parse(r#"{"command": "cargo test", "descri"#),
            Some(json!({ "command": "cargo test" }))
        );
        assert_eq!(
            parse(r#"{"command": "x", "description":"#),
            Some(json!({ "command": "x", "description": null }))
        );
        assert_eq!(
            parse(r#"{"file_path": "/a/b.rs", "edits": [{"old_string": "a\"b"#),
            Some(json!({ "file_path": "/a/b.rs", "edits": [{ "old_string": "a\"b" }] }))
        );
        assert_eq!(parse(r#"{"#), Some(json!({})));
        assert_eq!(parse(""), None);
    }

    #[test]
    fn a_changed_edit_is_told_to_claude_once() {
        let mut session = Session::new(PathBuf::from("/p"));
        let input = json!({ "file_path": "/p/a.txt", "old_string": "red", "new_string": "blue" });
        session.pending.push(Pending {
            id: "r1".into(),
            tool_use_id: "t1".into(),
            tool: "Edit".into(),
            display_name: "Edit".into(),
            description: None,
            input: input.clone(),
            kind: PendingKind::Edit(EditProposal {
                path: PathBuf::from("/p/a.txt"),
                original: Some("color = red\n".into()),
                proposed: Ok("color = blue\n".into()),
            }),
            suggestions: Vec::new(),
            blocked_path: None,
            reason: None,
            default_to_no: false,
            suppress_always_allow: false,
            from_agent: false,
        });
        let answer = Answer::Edit {
            text: "color = green\n".into(),
            remember: Vec::new(),
        };
        let response = Session::response(session.pending("r1").unwrap(), &answer);
        assert_eq!(response["updatedInput"]["new_string"], "color = green\n");
        session.resolve("r1", &answer);
        let hook = json!({ "hook_event_name": "PostToolUse", "tool_use_id": "t1" });
        let told = session.hook_response(&hook);
        let note = told["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(note.contains("color = green"));
        assert_eq!(session.hook_response(&hook), json!({}));
    }

    #[test]
    fn multi_select_answers_are_joined() {
        let pending = Pending {
            id: "r".into(),
            tool_use_id: "t".into(),
            tool: "AskUserQuestion".into(),
            display_name: "AskUserQuestion".into(),
            description: None,
            input: json!({ "questions": [] }),
            kind: PendingKind::Questions(Vec::new()),
            suggestions: Vec::new(),
            blocked_path: None,
            reason: None,
            default_to_no: false,
            suppress_always_allow: false,
            from_agent: false,
        };
        let answer = Answer::Answers(vec![
            ("Which?".into(), "A".into()),
            ("Which?".into(), "B".into()),
            ("Color?".into(), "Red".into()),
        ]);
        let response = Session::response(&pending, &answer);
        assert_eq!(response["updatedInput"]["answers"]["Which?"], "A, B");
        assert_eq!(response["updatedInput"]["answers"]["Color?"], "Red");
    }
}
