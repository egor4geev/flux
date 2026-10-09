//! The wire format of `claude` in host mode (`-p --input-format stream-json --output-format
//! stream-json --permission-prompt-tool stdio`): one JSON object per line in both directions.
//!
//! Parsing is tolerant, as the CLI adds frame types and fields between versions: a frame of an
//! unknown type is [`Incoming::Unknown`], a `system` frame of an unknown subtype is
//! [`System::Other`], unknown fields are ignored. Frames Flux writes are built by the functions at
//! the end of the module.

use base64::Engine;
use serde_json::{Value, json};

use crate::types::{
    BackgroundTask, Effort, ModelInfo, PermissionMode, RateLimits, SlashCommand, UserInput,
};

/// A frame from the CLI's stdout.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    System(System),
    /// An event of the message being generated (`--include-partial-messages`); subagents send
    /// none.
    Stream(StreamEvent),
    /// One finished block of Claude's message (each block comes as its own frame).
    Assistant(Message),
    /// The user's side as the CLI adds it: tool results, interrupt markers, a subagent's prompt,
    /// the compaction summary.
    User(Message),
    /// The end of a turn: exactly one per turn.
    Result(TurnResult),
    /// The subscription's usage windows after an API call.
    RateLimit(RateLimits),
    /// What became of a user message: queued, started, completed, cancelled.
    Lifecycle {
        /// The `uuid` Flux gave the message.
        message: String,
        state: Lifecycle,
    },
    /// A request of the CLI to Flux: answer exactly once ([`control_success`],
    /// [`control_error`]).
    Request {
        id: String,
        request: CliRequest,
    },
    /// The answer to one of Flux's control requests.
    Response {
        id: String,
        result: Result<Value, String>,
    },
    /// The CLI withdraws one of its requests (a permission prompt after an interrupt): close it,
    /// don't answer.
    Cancel {
        id: String,
    },
    KeepAlive,
    /// A frame this version doesn't know: kept for the log.
    Unknown(Value),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    Queued,
    Started,
    Completed,
    Cancelled,
}

/// `system` frames.
#[derive(Debug, Clone, PartialEq)]
pub enum System {
    /// At the start of every turn: the model, the mode, tools, commands.
    Init(Init),
    /// "requesting" before an API call, "compacting"; a changed permission mode.
    Status {
        status: Option<String>,
        permission_mode: Option<PermissionMode>,
    },
    /// Idle, running, waiting for the user (needs `CLAUDE_CODE_SDK_READS_SESSION_STATE=1`). Idle is
    /// the real "done": a background task that finishes starts a turn by itself.
    State(SessionState),
    /// A live estimate of hidden thinking, in tokens.
    ThinkingTokens(u64),
    /// The conversation was compacted.
    Compacted {
        /// "manual" (`/compact`) or "auto".
        trigger: String,
        pre_tokens: Option<u64>,
        post_tokens: Option<u64>,
    },
    /// A subagent or a background command: started, progress, a status change, the end.
    Task(TaskEvent),
    /// The whole live set of background tasks (replaces the previous one).
    BackgroundTasks(Vec<BackgroundTask>),
    /// A call refused without asking (the mode, a rule).
    PermissionDenied {
        tool_use_id: String,
        tool_name: String,
        message: String,
    },
    /// The session's title, made up by the CLI or set by `rename_session`.
    Title(String),
    /// The slash commands changed (replaces the previous list).
    Commands(Vec<SlashCommand>),
    /// The API call failed and is retried.
    ApiRetry {
        attempt: u32,
        max_retries: u32,
        delay_ms: u64,
        error: Option<String>,
    },
    /// A message of the CLI for the user (`notification`, `informational`).
    Notice {
        level: String,
        text: String,
    },
    Other {
        subtype: String,
        raw: Value,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Idle,
    Running,
    /// Waiting for the user: a permission prompt, a question, the plan.
    RequiresAction,
}

/// `system/init`: sent at the start of every turn.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Init {
    pub session_id: String,
    /// The resolved model id: "claude-opus-5-5".
    pub model: String,
    pub permission_mode: Option<PermissionMode>,
    pub effort: Option<Effort>,
    pub version: String,
    pub cwd: String,
    pub tools: Vec<String>,
    /// Commands that only make sense in the terminal CLI: hidden in the chat.
    pub terminal_commands: Vec<String>,
    /// MCP servers and their status ("connected", "failed", "needs-auth"…).
    pub mcp_servers: Vec<(String, String)>,
    pub capabilities: Vec<String>,
}

/// `system/task_*`: a subagent (`local_agent`) or a background command (`local_bash`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TaskEvent {
    /// "task_started", "task_progress", "task_updated", "task_notification".
    pub kind: String,
    pub task_id: String,
    /// The Agent or Bash call it belongs to.
    pub tool_use_id: Option<String>,
    pub description: Option<String>,
    pub task_type: Option<String>,
    /// running, completed, failed, killed, stopped…
    pub status: Option<String>,
    pub summary: Option<String>,
    pub last_tool: Option<String>,
    pub tokens: Option<u64>,
    pub tool_uses: Option<u64>,
    pub background: bool,
}

/// A streaming event (a raw Anthropic streaming event inside).
#[derive(Debug, Clone, PartialEq)]
pub struct StreamEvent {
    pub parent_tool_use_id: Option<String>,
    pub kind: StreamKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamKind {
    MessageStart {
        message_id: String,
    },
    BlockStart {
        index: usize,
        block: BlockStart,
    },
    TextDelta {
        index: usize,
        text: String,
    },
    ThinkingDelta {
        index: usize,
        text: String,
    },
    /// A piece of a tool call's input JSON.
    InputDelta {
        index: usize,
        json: String,
    },
    BlockStop {
        index: usize,
    },
    MessageDelta {
        stop_reason: Option<String>,
    },
    MessageStop,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BlockStart {
    Text,
    Thinking,
    ToolUse { id: String, name: String },
    Other,
}

/// An `assistant` or a `user` frame.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Message {
    pub uuid: Option<String>,
    /// The Agent call whose subagent sent this message; none — the main conversation.
    pub parent_tool_use_id: Option<String>,
    /// The API message id (assistant): the blocks of one message share it.
    pub message_id: Option<String>,
    pub content: Vec<Block>,
    /// The tool's own result object (`tool_use_result`): an edit's patch and original text, a
    /// command's stdout…
    pub tool_use_result: Option<Value>,
    /// The turn was interrupted: the text is cut short.
    pub aborted: bool,
    /// Why a request failed: "rate_limit", "authentication_failed", "billing_error"…
    pub error: Option<String>,
    /// A local slash command's reply (`/usage`, `/context`, `/compact`): model "<synthetic>".
    pub synthetic: bool,
    /// An echo of a message the host sent (`--replay-user-messages`).
    pub replay: bool,
    /// The compaction summary and other messages the CLI makes up.
    pub is_meta: bool,
    /// Tool calls of this message that didn't run, with why (`tool_result_meta`):
    /// "user-rejected", "permission-rule".
    pub non_executed: Vec<(String, String)>,
    /// The whole frame: extras of local commands (`usage_report`, `context_usage`).
    pub raw: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Text(String),
    /// Thinking: summarized text with `--thinking-display summarized`, otherwise empty.
    Thinking(String),
    RedactedThinking,
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        text: String,
        images: Vec<Image>,
        is_error: bool,
    },
    Image(Image),
    Other(Value),
}

/// A picture in a message: base64 as on the wire.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    pub media_type: String,
    pub data: String,
}

/// `result`: the end of a turn.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TurnResult {
    /// "success", "error_during_execution", "error_max_turns"…
    pub subtype: String,
    pub is_error: bool,
    pub text: Option<String>,
    pub duration_ms: u64,
    pub num_turns: u32,
    /// For the whole process so far (it grows from turn to turn).
    pub total_cost_usd: f64,
    /// "completed", "aborted_streaming", "aborted_tools", "max_turns", "prompt_too_long"…
    pub terminal_reason: Option<String>,
    /// The context window of the main model, from `modelUsage`.
    pub context_window: Option<u64>,
    /// Input tokens of the turn's last call (cached ones included): how full the context is.
    pub context_tokens: Option<u64>,
    pub errors: Vec<String>,
    /// The local slash command this turn ran ("cost", "context").
    pub local_command: Option<String>,
}

/// A request of the CLI to the host.
#[derive(Debug, Clone, PartialEq)]
pub enum CliRequest {
    /// A tool needs the user's permission (or, with `requires_user_interaction`, the user's
    /// answer: AskUserQuestion, ExitPlanMode).
    CanUseTool(ToolPermission),
    /// A hook Flux registered in `initialize`.
    HookCallback { callback_id: String, input: Value },
    /// A message to an in-process MCP server Flux registered ("sdk" servers).
    McpMessage { server: String, message: Value },
    /// Anything else (`elicitation`, `request_user_dialog`, newer ones): Flux declines.
    Other { subtype: String, raw: Value },
}

/// `can_use_tool`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ToolPermission {
    pub tool_name: String,
    /// "Edit", "Open In Editor" (an MCP tool).
    pub display_name: String,
    pub input: Value,
    pub tool_use_id: String,
    /// A ready label: the file name of an edit, the command's description, the URL.
    pub description: Option<String>,
    /// "Always allow" choices the CLI offers (`PermissionUpdate` objects, sent back as they are).
    pub suggestions: Vec<Value>,
    /// The path that made it ask (outside the project…).
    pub blocked_path: Option<String>,
    /// Why it asks ("Path is outside allowed working directories").
    pub reason: Option<String>,
    /// Claude's own UI is expected (AskUserQuestion, ExitPlanMode), not Allow / Deny.
    pub requires_user_interaction: bool,
    /// Don't preselect Allow.
    pub default_to_no: bool,
    /// Don't offer "always allow".
    pub suppress_always_allow: bool,
    /// The subagent that asks.
    pub agent_id: Option<String>,
    /// The MCP server of the tool.
    pub mcp_server: Option<String>,
}

/// A control request Flux sends to the CLI.
#[derive(Debug, Clone, PartialEq)]
pub enum HostRequest {
    /// The first frame: registers hooks and in-process MCP servers. `Value` — the request's fields
    /// besides `subtype`.
    Initialize(Value),
    /// Stops the running turn (the CLI's Esc).
    Interrupt,
    SetPermissionMode(PermissionMode),
    /// `None` — the account's default model.
    SetModel(Option<String>),
    /// `None` — the model's default effort (`apply_flag_settings {effortLevel}`).
    SetEffort(Option<Effort>),
    ListModels,
    /// The subscription's usage: 0–100 per window, reset times, the server's rows.
    GetUsage,
    /// How full the context window is (`detail: "summary"`, no token counting calls).
    GetContextUsage,
    GetBinaryVersion,
    /// Paths for an `@` mention.
    FileSuggestions(String),
    RenameSession(String),
    /// Takes back a queued message by its uuid.
    CancelQueued(String),
    /// Stops a background command or agent.
    StopTask(String),
}

impl HostRequest {
    /// The `request` object of the control request.
    pub fn to_json(&self) -> Value {
        match self {
            HostRequest::Initialize(fields) => {
                let mut request = json!({ "subtype": "initialize" });
                if let (Some(request), Some(fields)) = (request.as_object_mut(), fields.as_object())
                {
                    for (key, value) in fields {
                        request.insert(key.clone(), value.clone());
                    }
                }
                request
            }
            HostRequest::Interrupt => json!({ "subtype": "interrupt" }),
            HostRequest::SetPermissionMode(mode) => {
                json!({ "subtype": "set_permission_mode", "mode": mode.wire() })
            }
            HostRequest::SetModel(model) => json!({ "subtype": "set_model", "model": model }),
            HostRequest::SetEffort(effort) => json!({
                "subtype": "apply_flag_settings",
                "settings": { "effortLevel": effort.map(Effort::wire) },
            }),
            HostRequest::ListModels => json!({ "subtype": "list_models" }),
            HostRequest::GetUsage => json!({ "subtype": "get_usage", "skip_behaviors": true }),
            HostRequest::GetContextUsage => {
                json!({ "subtype": "get_context_usage", "detail": "summary" })
            }
            HostRequest::GetBinaryVersion => json!({ "subtype": "get_binary_version" }),
            HostRequest::FileSuggestions(query) => {
                json!({ "subtype": "file_suggestions", "query": query })
            }
            HostRequest::RenameSession(title) => {
                json!({ "subtype": "rename_session", "title": title, "source": "host" })
            }
            HostRequest::CancelQueued(uuid) => {
                json!({ "subtype": "cancel_async_message", "message_uuid": uuid })
            }
            HostRequest::StopTask(task_id) => json!({ "subtype": "stop_task", "task_id": task_id }),
        }
    }
}

// --- Reading ---

/// One line of stdout; `None` for a line that isn't JSON (the CLI may print a warning).
pub fn parse_line(line: &str) -> Option<Incoming> {
    let value: Value = serde_json::from_str(line.trim()).ok()?;
    Some(parse(value))
}

/// A frame by its `type` (and `subtype`).
pub fn parse(value: Value) -> Incoming {
    match value["type"].as_str().unwrap_or("") {
        "system" => Incoming::System(parse_system(&value)),
        "stream_event" => Incoming::Stream(parse_stream(&value)),
        "assistant" => Incoming::Assistant(parse_message(&value)),
        "user" => Incoming::User(parse_message(&value)),
        "result" => Incoming::Result(parse_result(&value)),
        "rate_limit_event" => {
            Incoming::RateLimit(RateLimits::from_event(&value["rate_limit_info"]))
        }
        "command_lifecycle" => match lifecycle(value["state"].as_str()) {
            Some(state) => Incoming::Lifecycle {
                message: text(&value["command_uuid"]),
                state,
            },
            None => Incoming::Unknown(value),
        },
        "control_request" => Incoming::Request {
            id: text(&value["request_id"]),
            request: parse_cli_request(&value["request"]),
        },
        "control_response" => {
            let response = &value["response"];
            let id = text(&response["request_id"]);
            let result = if response["subtype"] == "error" {
                Err(response["error"]
                    .as_str()
                    .unwrap_or("The request failed")
                    .to_string())
            } else {
                Ok(response["response"].clone())
            };
            Incoming::Response { id, result }
        }
        "control_cancel_request" => Incoming::Cancel {
            id: text(&value["request_id"]),
        },
        "keep_alive" => Incoming::KeepAlive,
        _ => Incoming::Unknown(value),
    }
}

fn lifecycle(state: Option<&str>) -> Option<Lifecycle> {
    Some(match state? {
        "queued" => Lifecycle::Queued,
        "started" => Lifecycle::Started,
        "completed" => Lifecycle::Completed,
        "cancelled" => Lifecycle::Cancelled,
        _ => return None,
    })
}

fn parse_system(value: &Value) -> System {
    let subtype = value["subtype"].as_str().unwrap_or("");
    match subtype {
        "init" => System::Init(Init {
            session_id: text(&value["session_id"]),
            model: text(&value["model"]),
            permission_mode: value["permissionMode"]
                .as_str()
                .and_then(PermissionMode::from_wire),
            effort: value["effort"].as_str().and_then(Effort::from_wire),
            version: text(&value["claude_code_version"]),
            cwd: text(&value["cwd"]),
            tools: strings(&value["tools"]),
            terminal_commands: strings(&value["terminal_slash_commands"]),
            mcp_servers: value["mcp_servers"]
                .as_array()
                .map(|servers| {
                    servers
                        .iter()
                        .map(|server| (text(&server["name"]), text(&server["status"])))
                        .collect()
                })
                .unwrap_or_default(),
            capabilities: strings(&value["capabilities"]),
        }),
        "status" => System::Status {
            status: value["status"].as_str().map(str::to_string),
            permission_mode: value["permissionMode"]
                .as_str()
                .and_then(PermissionMode::from_wire),
        },
        "session_state_changed" => match value["state"].as_str() {
            Some("idle") => System::State(SessionState::Idle),
            Some("running") => System::State(SessionState::Running),
            Some("requires_action") => System::State(SessionState::RequiresAction),
            _ => other(subtype, value),
        },
        "thinking_tokens" => {
            System::ThinkingTokens(value["estimated_tokens"].as_u64().unwrap_or(0))
        }
        "compact_boundary" => {
            let metadata = &value["compact_metadata"];
            System::Compacted {
                trigger: metadata["trigger"].as_str().unwrap_or("auto").to_string(),
                pre_tokens: metadata["pre_tokens"].as_u64(),
                post_tokens: metadata["post_tokens"].as_u64(),
            }
        }
        "task_started" | "task_progress" | "task_updated" | "task_notification" => {
            let usage = &value["usage"];
            System::Task(TaskEvent {
                kind: subtype.to_string(),
                task_id: text(&value["task_id"]),
                tool_use_id: optional(&value["tool_use_id"]),
                description: optional(&value["description"]),
                task_type: optional(&value["task_type"]),
                status: optional(&value["status"]).or_else(|| optional(&value["patch"]["status"])),
                summary: optional(&value["summary"]),
                last_tool: optional(&value["last_tool_name"]),
                tokens: usage["total_tokens"].as_u64(),
                tool_uses: usage["tool_uses"].as_u64(),
                background: value["is_backgrounded"].as_bool().unwrap_or(false),
            })
        }
        "background_tasks_changed" => System::BackgroundTasks(
            value["tasks"]
                .as_array()
                .map(|tasks| {
                    tasks
                        .iter()
                        .map(|task| BackgroundTask {
                            task_id: text(&task["task_id"]),
                            task_type: text(&task["task_type"]),
                            description: text(&task["description"]),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        ),
        "permission_denied" => System::PermissionDenied {
            tool_use_id: text(&value["tool_use_id"]),
            tool_name: text(&value["tool_name"]),
            message: text(&value["message"]),
        },
        "session_title_changed" => System::Title(text(&value["title"])),
        "commands_changed" => System::Commands(commands(&value["commands"])),
        "api_retry" => System::ApiRetry {
            attempt: value["attempt"].as_u64().unwrap_or(0) as u32,
            max_retries: value["max_retries"].as_u64().unwrap_or(0) as u32,
            delay_ms: value["retry_delay_ms"].as_u64().unwrap_or(0),
            error: optional(&value["error"]),
        },
        "notification" => System::Notice {
            level: value["priority"].as_str().unwrap_or("info").to_string(),
            text: text(&value["text"]),
        },
        "informational" => System::Notice {
            level: value["level"].as_str().unwrap_or("info").to_string(),
            text: text(&value["content"]),
        },
        _ => other(subtype, value),
    }
}

fn other(subtype: &str, value: &Value) -> System {
    System::Other {
        subtype: subtype.to_string(),
        raw: value.clone(),
    }
}

/// The slash commands of `initialize` or `commands_changed`.
pub fn commands(value: &Value) -> Vec<SlashCommand> {
    value
        .as_array()
        .map(|commands| {
            commands
                .iter()
                .filter_map(SlashCommand::from_json)
                .collect()
        })
        .unwrap_or_default()
}

/// The models of `initialize` or `list_models`.
pub fn models(value: &Value) -> Vec<ModelInfo> {
    value
        .as_array()
        .map(|models| models.iter().filter_map(ModelInfo::from_json).collect())
        .unwrap_or_default()
}

fn parse_stream(value: &Value) -> StreamEvent {
    let event = &value["event"];
    let index = event["index"].as_u64().unwrap_or(0) as usize;
    let kind = match event["type"].as_str().unwrap_or("") {
        "message_start" => StreamKind::MessageStart {
            message_id: text(&event["message"]["id"]),
        },
        "content_block_start" => {
            let block = &event["content_block"];
            StreamKind::BlockStart {
                index,
                block: match block["type"].as_str().unwrap_or("") {
                    "text" => BlockStart::Text,
                    "thinking" => BlockStart::Thinking,
                    "tool_use" => BlockStart::ToolUse {
                        id: text(&block["id"]),
                        name: text(&block["name"]),
                    },
                    _ => BlockStart::Other,
                },
            }
        }
        "content_block_delta" => {
            let delta = &event["delta"];
            match delta["type"].as_str().unwrap_or("") {
                "text_delta" => StreamKind::TextDelta {
                    index,
                    text: text(&delta["text"]),
                },
                "thinking_delta" => StreamKind::ThinkingDelta {
                    index,
                    text: text(&delta["thinking"]),
                },
                "input_json_delta" => StreamKind::InputDelta {
                    index,
                    json: text(&delta["partial_json"]),
                },
                _ => StreamKind::Other,
            }
        }
        "content_block_stop" => StreamKind::BlockStop { index },
        "message_delta" => StreamKind::MessageDelta {
            stop_reason: optional(&event["delta"]["stop_reason"]),
        },
        "message_stop" => StreamKind::MessageStop,
        _ => StreamKind::Other,
    };
    StreamEvent {
        parent_tool_use_id: optional(&value["parent_tool_use_id"]),
        kind,
    }
}

fn parse_message(value: &Value) -> Message {
    let message = &value["message"];
    let content = match &message["content"] {
        Value::String(text) => vec![Block::Text(text.clone())],
        Value::Array(blocks) => blocks.iter().map(parse_block).collect(),
        _ => Vec::new(),
    };
    Message {
        uuid: optional(&value["uuid"]),
        parent_tool_use_id: optional(&value["parent_tool_use_id"]),
        message_id: optional(&message["id"]),
        content,
        tool_use_result: value
            .get("tool_use_result")
            .filter(|result| !result.is_null())
            .cloned(),
        aborted: value["aborted"].as_bool().unwrap_or(false),
        error: optional(&value["error"]),
        synthetic: message["model"] == "<synthetic>",
        replay: value["isReplay"].as_bool().unwrap_or(false),
        is_meta: value["isSynthetic"].as_bool().unwrap_or(false),
        non_executed: value["tool_result_meta"]
            .as_array()
            .map(|meta| {
                meta.iter()
                    .map(|item| (text(&item["id"]), text(&item["non_execution_kind"])))
                    .collect()
            })
            .unwrap_or_default(),
        raw: value.clone(),
    }
}

fn parse_block(block: &Value) -> Block {
    match block["type"].as_str().unwrap_or("") {
        "text" => Block::Text(text(&block["text"])),
        "thinking" => Block::Thinking(text(&block["thinking"])),
        "redacted_thinking" => Block::RedactedThinking,
        "tool_use" => Block::ToolUse {
            id: text(&block["id"]),
            name: text(&block["name"]),
            input: block["input"].clone(),
        },
        "tool_result" => {
            let (text, images) = match &block["content"] {
                Value::String(text) => (text.clone(), Vec::new()),
                Value::Array(parts) => {
                    let mut text = String::new();
                    let mut images = Vec::new();
                    for part in parts {
                        match part["type"].as_str() {
                            Some("text") => {
                                if !text.is_empty() {
                                    text.push('\n');
                                }
                                text.push_str(part["text"].as_str().unwrap_or(""));
                            }
                            Some("image") => images.extend(image(part)),
                            _ => {}
                        }
                    }
                    (text, images)
                }
                _ => (String::new(), Vec::new()),
            };
            Block::ToolResult {
                tool_use_id: self::text(&block["tool_use_id"]),
                text,
                images,
                is_error: block["is_error"].as_bool().unwrap_or(false),
            }
        }
        "image" => match image(block) {
            Some(image) => Block::Image(image),
            None => Block::Other(block.clone()),
        },
        _ => Block::Other(block.clone()),
    }
}

fn image(block: &Value) -> Option<Image> {
    let source = &block["source"];
    Some(Image {
        media_type: source["media_type"].as_str()?.to_string(),
        data: source["data"].as_str()?.to_string(),
    })
}

fn parse_result(value: &Value) -> TurnResult {
    // The main model's window: the largest one of the turn's models.
    let models = value["modelUsage"].as_object();
    let context_window = models.and_then(|models| {
        models
            .values()
            .filter_map(|usage| usage["contextWindow"].as_u64())
            .max()
    });
    let usage = &value["usage"];
    let context_tokens = usage["input_tokens"].as_u64().map(|input| {
        input
            + usage["cache_read_input_tokens"].as_u64().unwrap_or(0)
            + usage["cache_creation_input_tokens"].as_u64().unwrap_or(0)
    });
    TurnResult {
        subtype: text(&value["subtype"]),
        is_error: value["is_error"].as_bool().unwrap_or(false),
        text: optional(&value["result"]),
        duration_ms: value["duration_ms"].as_u64().unwrap_or(0),
        num_turns: value["num_turns"].as_u64().unwrap_or(0) as u32,
        total_cost_usd: value["total_cost_usd"].as_f64().unwrap_or(0.),
        terminal_reason: optional(&value["terminal_reason"]),
        context_window,
        context_tokens,
        errors: strings(&value["errors"]),
        local_command: optional(&value["local_command"]),
    }
}

fn parse_cli_request(request: &Value) -> CliRequest {
    match request["subtype"].as_str().unwrap_or("") {
        "can_use_tool" => CliRequest::CanUseTool(ToolPermission {
            tool_name: text(&request["tool_name"]),
            display_name: request["display_name"]
                .as_str()
                .or_else(|| request["tool_name"].as_str())
                .unwrap_or("")
                .to_string(),
            input: request["input"].clone(),
            tool_use_id: text(&request["tool_use_id"]),
            description: optional(&request["description"]),
            suggestions: request["permission_suggestions"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
            blocked_path: optional(&request["blocked_path"]),
            reason: optional(&request["decision_reason"]),
            requires_user_interaction: request["requires_user_interaction"]
                .as_bool()
                .unwrap_or(false),
            default_to_no: request["default_to_no"].as_bool().unwrap_or(false),
            suppress_always_allow: request["suppress_always_allow_rule"]
                .as_bool()
                .unwrap_or(false),
            agent_id: optional(&request["agent_id"]),
            mcp_server: optional(&request["mcp_server"]["name"]),
        }),
        "hook_callback" => CliRequest::HookCallback {
            callback_id: text(&request["callback_id"]),
            input: request["input"].clone(),
        },
        "mcp_message" => CliRequest::McpMessage {
            server: text(&request["server_name"]),
            message: request["message"].clone(),
        },
        subtype => CliRequest::Other {
            subtype: subtype.to_string(),
            raw: request.clone(),
        },
    }
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or("").to_string()
}

fn optional(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| Some(item.as_str()?.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

// --- Writing ---

/// A user message (`uuid` — the id the CLI's lifecycle frames refer to).
pub fn user_message(uuid: &str, input: &UserInput) -> Value {
    let mut content = Vec::new();
    for image in &input.images {
        content.push(json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": image.media_type,
                "data": base64::engine::general_purpose::STANDARD.encode(&image.data),
            },
        }));
    }
    if !input.text.is_empty() || content.is_empty() {
        content.push(json!({ "type": "text", "text": input.text }));
    }
    let mut message = json!({
        "type": "user",
        "message": { "role": "user", "content": content },
        "parent_tool_use_id": null,
        "session_id": "",
        "uuid": uuid,
    });
    if let Some(priority) = input.priority {
        message["priority"] = Value::from(priority.wire());
    }
    message
}

/// A control request of the host.
pub fn control_request(id: &str, request: &HostRequest) -> Value {
    json!({ "type": "control_request", "request_id": id, "request": request.to_json() })
}

/// A successful answer to one of the CLI's requests.
pub fn control_success(id: &str, response: Value) -> Value {
    json!({
        "type": "control_response",
        "response": { "subtype": "success", "request_id": id, "response": response },
    })
}

/// A refusal of one of the CLI's requests (an unknown subtype, an MCP server Flux doesn't have).
pub fn control_error(id: &str, error: &str) -> Value {
    json!({
        "type": "control_response",
        "response": { "subtype": "error", "request_id": id, "error": error },
    })
}
