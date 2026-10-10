//! Saved sessions of a project: `~/.claude/projects/<slug>/<session id>.jsonl`, where the slug is
//! the absolute project path with every character other than a letter or a digit replaced by `-`
//! (part 9.2: the session history, Resume, sessions after Flux restarts). The CLI writes them —
//! the terminal's sessions and Flux's alike; Flux only reads them.
//!
//! A transcript is one JSON object per line (PROTOCOL.md §11): the conversation's `user`,
//! `assistant`, `system` and `attachment` lines, chained by `parentUuid` (a rewind starts a new
//! branch: the active one ends with the last line), and bookkeeping lines (`custom-title`,
//! `ai-title`, `last-prompt`, `queue-operation`…). A subagent's conversation is a file of its own,
//! `<session id>/subagents/agent-<id>.jsonl`.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use base64::Engine;
use serde_json::{Value, json};

use crate::process::ProcessEvent;
use crate::protocol;
use crate::session::{Change, EntryKind, Notice, Session, TurnSummary};
use crate::types::{ImageAttachment, parse_time};

/// How much of a transcript's beginning and end the history reads: a long one is tens of
/// megabytes, while the first prompt is near the start and the title is written after every turn.
const HEAD_BYTES: u64 = 64 * 1024;
const TAIL_BYTES: u64 = 128 * 1024;
/// A title made of the first prompt is cut to this many characters.
const TITLE_CHARS: usize = 80;
/// A subagent's transcript larger than this isn't read into the conversation (its call still
/// shows its result).
const SUBAGENT_LIMIT: u64 = 8 * 1024 * 1024;

/// A saved session, as the history lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedSession {
    pub id: String,
    /// The title the user gave (`custom-title`), else the CLI's (`ai-title`, `summary`), else the
    /// first prompt, shortened.
    pub title: String,
    /// The first thing the user wrote (not a command, not a tool result).
    pub prompt: Option<String>,
    /// The git branch the session started on.
    pub branch: Option<String>,
    /// When the transcript was last written.
    pub modified: SystemTime,
    /// The transcript's size in bytes (long sessions are many megabytes).
    pub size: u64,
    pub path: PathBuf,
}

/// The folder of the CLI's transcripts: `$CLAUDE_CONFIG_DIR/projects`, by default
/// `~/.claude/projects`.
pub fn projects_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|dir| !dir.is_empty()) {
        return Some(PathBuf::from(dir).join("projects"));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".claude").join("projects"))
}

/// The folder of a project's transcripts under `projects`.
pub fn project_dir(projects: &Path, root: &Path) -> PathBuf {
    let slug: String = root
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    projects.join(slug)
}

/// The project's saved sessions, the most recent first. Reads only the beginning and the end of
/// each transcript: a long one is tens of megabytes. A transcript without a prompt of the user (a
/// session that never got one) isn't listed.
pub fn list(projects: &Path, root: &Path) -> Vec<SavedSession> {
    let Ok(entries) = fs::read_dir(project_dir(projects, root)) else {
        return Vec::new();
    };
    let mut sessions: Vec<SavedSession> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl") && path.is_file())
        .filter_map(|path| summarize(&path))
        .collect();
    sessions.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.id.cmp(&b.id)));
    sessions
}

/// What the history shows of one transcript.
fn summarize(path: &Path) -> Option<SavedSession> {
    let id = path.file_stem()?.to_str()?.to_string();
    let metadata = fs::metadata(path).ok()?;
    let size = metadata.len();
    let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let mut file = File::open(path).ok()?;
    let head = read_lines(&mut file, 0, HEAD_BYTES.min(size), size);
    let tail = if size > HEAD_BYTES {
        let start = size.saturating_sub(TAIL_BYTES).max(HEAD_BYTES);
        read_lines(&mut file, start, size - start, size)
    } else {
        Vec::new()
    };

    let mut prompt = None;
    let mut branch = None;
    let mut titles = Titles::default();
    let mut last_prompt = None;
    for line in head.iter().chain(&tail) {
        titles.read(line);
        if line["type"] == "last-prompt"
            && let Some(text) = line["lastPrompt"].as_str().filter(|text| !text.is_empty())
        {
            last_prompt = Some(text.to_string());
        }
    }
    for line in &head {
        if branch.is_none() {
            branch = line["gitBranch"]
                .as_str()
                .filter(|branch| !branch.is_empty())
                .map(str::to_string);
        }
        if prompt.is_none() {
            prompt = first_prompt(line);
        }
    }
    // The first prompt didn't fit in the head (a long paste, a picture): the last one stands in.
    let prompt = prompt.or(last_prompt);
    let title = titles.best().or_else(|| prompt.as_deref().map(shorten))?;
    Some(SavedSession {
        id,
        title,
        prompt,
        branch,
        modified,
        size,
        path: path.to_path_buf(),
    })
}

/// The whole lines of `length` bytes of the file from `start`: a line cut at either end is left
/// out (unless it is the file's own start or end).
fn read_lines(file: &mut File, start: u64, length: u64, size: u64) -> Vec<Value> {
    let mut bytes = Vec::with_capacity(length as usize);
    if file.seek(SeekFrom::Start(start)).is_err()
        || file.take(length).read_to_end(&mut bytes).is_err()
    {
        return Vec::new();
    }
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if start + length < size {
        let end = text.rfind('\n').map_or(0, |end| end + 1);
        text.truncate(end);
    }
    let text = if start > 0 {
        text.find('\n').map_or("", |at| &text[at + 1..])
    } else {
        &text[..]
    };
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// The titles a transcript carries; the last of each kind counts.
#[derive(Default)]
struct Titles {
    custom: Option<String>,
    ai: Option<String>,
    summary: Option<String>,
}

impl Titles {
    fn read(&mut self, line: &Value) {
        let text = |key: &str| {
            line[key]
                .as_str()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
        };
        match line["type"].as_str() {
            Some("custom-title") => self.custom = text("customTitle").or(self.custom.take()),
            Some("ai-title") => self.ai = text("aiTitle").or(self.ai.take()),
            Some("summary") => self.summary = text("summary").or(self.summary.take()),
            _ => {}
        }
    }

    fn best(&self) -> Option<String> {
        self.custom
            .clone()
            .or_else(|| self.ai.clone())
            .or_else(|| self.summary.clone())
    }
}

/// A prompt as a title: one line, at most [`TITLE_CHARS`] characters.
fn shorten(prompt: &str) -> String {
    let line = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= TITLE_CHARS {
        return line;
    }
    let cut: String = line.chars().take(TITLE_CHARS - 1).collect();
    format!("{}…", cut.trim_end())
}

/// The user's own prompt in a line of the conversation: not a command, a command's output, a tool
/// result, a notification of a background task or another injected message.
fn first_prompt(line: &Value) -> Option<String> {
    match user_line(line)? {
        UserLine::Prompt { text, .. } if !text.is_empty() => Some(text),
        _ => None,
    }
}

/// What a `user` line of the conversation is.
enum UserLine {
    /// What the user wrote (a message, a queued one, `! command` of the terminal's bash mode).
    Prompt {
        text: String,
        images: Vec<ImageAttachment>,
    },
    /// A slash command the user ran: `/model opus`.
    Command(String),
    /// What a local command or the terminal's bash mode printed.
    Output(String),
    /// Tool results (and the interruption marks): the session model pairs them with the calls.
    Frame,
}

/// Wrappers of messages the CLI puts in the conversation by itself.
const INJECTED: [&str; 6] = [
    "<task-notification>",
    "<local-command-caveat>",
    "<system-reminder>",
    "<user-memory-input>",
    "<scheduled-task",
    "<teammate-message",
];

fn user_line(line: &Value) -> Option<UserLine> {
    if line["type"] != "user"
        || line["isSidechain"] == true
        || line["isMeta"] == true
        || line["isCompactSummary"] == true
        || line["isVisibleInTranscriptOnly"] == true
    {
        return None;
    }
    let content = &line["message"]["content"];
    let blocks: Vec<&Value> = match content {
        Value::Array(blocks) => blocks.iter().collect(),
        Value::String(_) => vec![content],
        _ => return None,
    };
    let texts: Vec<&str> = blocks
        .iter()
        .filter_map(|block| match block {
            Value::String(text) => Some(text.as_str()),
            block if block["type"] == "text" => block["text"].as_str(),
            _ => None,
        })
        .collect();
    let interrupted = texts
        .iter()
        .any(|text| text.starts_with("[Request interrupted by user"));
    if interrupted || blocks.iter().any(|block| block["type"] == "tool_result") {
        return Some(UserLine::Frame);
    }
    let text = texts.join("\n");
    let trimmed = text.trim();
    if INJECTED.iter().any(|tag| trimmed.starts_with(tag)) {
        return None;
    }
    if let Some(name) = tag(trimmed, "command-name") {
        let args = tag(trimmed, "command-args").unwrap_or_default();
        let name = if name.starts_with('/') {
            name
        } else {
            format!("/{name}")
        };
        return Some(UserLine::Command(
            format!("{name} {args}").trim_end().to_string(),
        ));
    }
    if trimmed.starts_with("<local-command-stdout>")
        || trimmed.starts_with("<local-command-stderr>")
        || trimmed.starts_with("<bash-stdout>")
        || trimmed.starts_with("<bash-stderr>")
    {
        let output = [
            "local-command-stdout",
            "local-command-stderr",
            "bash-stdout",
            "bash-stderr",
        ]
        .iter()
        .filter_map(|name| tag(trimmed, name))
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
        return Some(UserLine::Output(strip_ansi(&output)));
    }
    let text = match tag(trimmed, "bash-input") {
        Some(command) => format!("! {command}"),
        None => trimmed.to_string(),
    };
    let images = blocks
        .iter()
        .filter(|block| block["type"] == "image")
        .filter_map(|block| image(block))
        .collect();
    Some(UserLine::Prompt { text, images })
}

/// The text inside `<name>…</name>`.
fn tag(text: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close).map_or(text.len(), |end| start + end);
    Some(text[start..end].trim().to_string())
}

/// Terminal colors out of a command's output (`/cost` prints with escapes).
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

fn image(block: &Value) -> Option<ImageAttachment> {
    let source = &block["source"];
    let data = base64::engine::general_purpose::STANDARD
        .decode(source["data"].as_str()?)
        .ok()?;
    Some(ImageAttachment {
        media_type: source["media_type"].as_str()?.to_string(),
        data,
    })
}

/// A saved session as the chat shows it: the conversation (the user's messages, Claude's text and
/// thinking, tool calls with their results, subagents inside their calls), the title, the files
/// Claude changed with their text before the first change. No process runs: the status is
/// [`crate::Status::Idle`] and the next message resumes the session (`--resume`).
pub fn load(path: &Path, cwd: PathBuf) -> io::Result<Session> {
    let bytes = fs::read(path)?;
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<Value> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let mut titles = Titles::default();
    for line in &lines {
        titles.read(line);
    }
    let mut reader = Reader::new(Session::new(cwd));
    // The file's name is the id `--resume` takes (a forked session's lines may carry the id of
    // the one it came from).
    reader.session.info.session_id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_string)
        .or_else(|| {
            lines
                .iter()
                .find_map(|line| line["sessionId"].as_str().map(str::to_string))
        });
    for index in active_chain(&lines) {
        reader.line(&lines[index], None);
    }
    reader.finish_turn(None);
    // Subagents: each one's conversation goes into its call.
    let agents = reader.agents.clone();
    if let Some(dir) = path
        .file_stem()
        .map(|stem| path.with_file_name(stem).join("subagents"))
    {
        for (tool_use_id, agent_id) in agents {
            let file = dir.join(format!("agent-{agent_id}.jsonl"));
            if fs::metadata(&file).is_ok_and(|meta| meta.len() <= SUBAGENT_LIMIT)
                && let Ok(text) = fs::read_to_string(&file)
            {
                for line in text.lines().filter_map(|line| serde_json::from_str(line).ok()) {
                    reader.line(&line, Some(&tool_use_id));
                }
            }
        }
    }
    let mut session = reader.session;
    session.finish_restore();
    session.info.title = titles.best().or_else(|| reader.first_prompt.as_deref().map(shorten));
    session.info.model = reader.model;
    Ok(session)
}

/// The lines of the active branch of the conversation, oldest first: from the last message back
/// through `parentUuid` (past a compaction — through `logicalParentUuid`). Lines of subagents and
/// of abandoned branches (a rewind) are left out.
fn active_chain(lines: &[Value]) -> Vec<usize> {
    let mut by_uuid: HashMap<&str, usize> = HashMap::new();
    let mut leaf = None;
    for (index, line) in lines.iter().enumerate() {
        let Some(uuid) = line["uuid"].as_str() else {
            continue;
        };
        if !matches!(
            line["type"].as_str(),
            Some("user" | "assistant" | "system" | "attachment")
        ) || line["isSidechain"] == true
        {
            continue;
        }
        by_uuid.insert(uuid, index);
        leaf = Some(index);
    }
    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    let mut next = leaf;
    while let Some(index) = next {
        if !seen.insert(index) {
            break;
        }
        chain.push(index);
        let line = &lines[index];
        next = line["parentUuid"]
            .as_str()
            .or_else(|| line["logicalParentUuid"].as_str())
            .and_then(|uuid| by_uuid.get(uuid).copied());
    }
    chain.reverse();
    chain
}

/// Turns transcript lines into the session model, the way the live frames do.
struct Reader {
    session: Session,
    /// The model of the last answer.
    model: Option<String>,
    first_prompt: Option<String>,
    /// The turn going on: when its prompt was written, when its last line was, whether it has
    /// anything of Claude's, whether it was interrupted.
    turn: Option<Turn>,
    /// Agent calls of the main conversation → their subagent's id.
    agents: Vec<(String, String)>,
}

struct Turn {
    started: Option<SystemTime>,
    last: Option<SystemTime>,
    answered: bool,
    interrupted: bool,
}

impl Reader {
    fn new(session: Session) -> Self {
        Reader {
            session,
            model: None,
            first_prompt: None,
            turn: None,
            agents: Vec::new(),
        }
    }

    /// One line of the conversation; `parent` — the Agent call whose subagent wrote it.
    fn line(&mut self, line: &Value, parent: Option<&str>) {
        let at = line["timestamp"].as_str().and_then(parse_time);
        self.read(line, parent, at);
        // The turn lasts until its last message (a compaction later doesn't lengthen it; a new
        // prompt has started a turn of its own by now).
        if parent.is_none()
            && matches!(
                line["type"].as_str(),
                Some("user" | "assistant" | "attachment")
            )
            && let Some(turn) = &mut self.turn
        {
            turn.last = at.or(turn.last);
        }
    }

    fn read(&mut self, line: &Value, parent: Option<&str>, at: Option<SystemTime>) {
        match line["type"].as_str() {
            Some("user") if parent.is_some() => self.frame(line, parent, at),
            Some("user") => match user_line(line) {
                Some(UserLine::Prompt { text, images }) => {
                    if text.is_empty() && images.is_empty() {
                        return;
                    }
                    self.start_turn(at);
                    if self.first_prompt.is_none() && !text.is_empty() {
                        self.first_prompt = Some(text.clone());
                    }
                    let uuid = line["uuid"].as_str().map(str::to_string);
                    self.session.restore_user(uuid, text, images);
                }
                Some(UserLine::Command(command)) => {
                    self.start_turn(at);
                    let uuid = line["uuid"].as_str().map(str::to_string);
                    self.session.restore_user(uuid, command, Vec::new());
                }
                Some(UserLine::Output(text)) => {
                    if !text.trim().is_empty() {
                        self.session
                            .restore_entry(EntryKind::Notice(Notice::LocalCommand {
                                text,
                                raw: Value::Null,
                            }));
                    }
                }
                Some(UserLine::Frame) => {
                    self.frame(line, None, at);
                    if self.session.last_entry() == Some(&EntryKind::Notice(Notice::Interrupted))
                        && let Some(turn) = &mut self.turn
                    {
                        turn.interrupted = true;
                    }
                }
                None => {}
            },
            Some("assistant") => {
                if line["isApiErrorMessage"] == true {
                    let text = line["message"]["content"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .find_map(|block| block["text"].as_str())
                        .unwrap_or_default()
                        .to_string();
                    self.session.restore_entry(EntryKind::Notice(Notice::Error {
                        kind: line["error"].as_str().unwrap_or("api_error").to_string(),
                        text,
                    }));
                    return;
                }
                if parent.is_none() {
                    if let Some(model) = line["message"]["model"]
                        .as_str()
                        .filter(|model| !model.is_empty() && !model.starts_with('<'))
                    {
                        self.model = Some(model.to_string());
                    }
                    if let Some(turn) = &mut self.turn {
                        turn.answered = true;
                    }
                }
                self.frame(line, parent, at);
            }
            Some("system") if parent.is_none() => self.system(line),
            Some("attachment") if parent.is_none() => {
                // A message the user wrote while Claude worked reached it in the middle of the
                // turn.
                let attachment = &line["attachment"];
                if attachment["type"] == "queued_command"
                    && attachment["commandMode"] == "prompt"
                    && attachment["isMeta"] != true
                    && let Some(text) = attachment["prompt"]
                        .as_str()
                        .map(str::trim)
                        .filter(|text| !text.is_empty())
                {
                    let uuid = line["uuid"].as_str().map(str::to_string);
                    self.session.restore_user(uuid, text.to_string(), Vec::new());
                }
            }
            _ => {}
        }
    }

    /// A message the live session would get as a frame: Claude's blocks, tool results.
    fn frame(&mut self, line: &Value, parent: Option<&str>, at: Option<SystemTime>) {
        let mut frame = json!({
            "type": line["type"],
            "message": line["message"],
            "uuid": line["uuid"],
            "parent_tool_use_id": parent,
        });
        if let Some(result) = line.get("toolUseResult").filter(|result| !result.is_null()) {
            frame["tool_use_result"] = result.clone();
        }
        let changes = self
            .session
            .apply(&ProcessEvent::Frame(Box::new(protocol::parse(frame))));
        for change in changes {
            if let Change::FilesChanged(paths) = change
                && let Some(at) = at
            {
                for path in paths {
                    self.session.restore_changed_at(&path, at);
                }
            }
        }
        // An Agent call of the main conversation: its subagent's transcript comes later.
        if parent.is_none()
            && let Some(result) = line.get("toolUseResult")
            && let Some(agent) = result["agentId"].as_str()
            && let Some(id) = line["message"]["content"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|block| block["type"] == "tool_result")
                .and_then(|block| block["tool_use_id"].as_str())
        {
            self.agents.push((id.to_string(), agent.to_string()));
        }
    }

    fn system(&mut self, line: &Value) {
        let content = || line["content"].as_str().unwrap_or_default().trim().to_string();
        match line["subtype"].as_str() {
            Some("turn_duration") => {
                let duration = line["durationMs"].as_u64().map(Duration::from_millis);
                self.finish_turn(duration);
            }
            Some("compact_boundary") => {
                // The turn before the compaction ends above its notice.
                self.finish_turn(None);
                let metadata = &line["compactMetadata"];
                self.session
                    .restore_entry(EntryKind::Notice(Notice::Compacted {
                        trigger: metadata["trigger"].as_str().unwrap_or("manual").to_string(),
                        pre_tokens: metadata["preTokens"].as_u64(),
                        post_tokens: metadata["postTokens"].as_u64(),
                    }));
            }
            Some("local_command") => {
                let text = content();
                if let Some(name) = tag(&text, "command-name") {
                    let args = tag(&text, "command-args").unwrap_or_default();
                    let command = format!("{name} {args}").trim_end().to_string();
                    self.start_turn(line["timestamp"].as_str().and_then(parse_time));
                    self.session.restore_user(None, command, Vec::new());
                } else if let Some(output) = tag(&text, "local-command-stdout") {
                    let output = strip_ansi(&output);
                    if !output.trim().is_empty() {
                        self.session
                            .restore_entry(EntryKind::Notice(Notice::LocalCommand {
                                text: output,
                                raw: Value::Null,
                            }));
                    }
                }
            }
            Some("informational" | "model_refusal_fallback" | "model_refusal_no_fallback") => {
                let text = content();
                if !text.is_empty() {
                    let level = line["level"].as_str().unwrap_or("info").to_string();
                    self.session
                        .restore_entry(EntryKind::Notice(Notice::Info { level, text }));
                }
            }
            _ => {}
        }
    }

    /// A new prompt: the turn before it ends, if it hasn't yet.
    fn start_turn(&mut self, at: Option<SystemTime>) {
        self.finish_turn(None);
        self.turn = Some(Turn {
            started: at,
            last: at,
            answered: false,
            interrupted: false,
        });
    }

    /// The turn's end line: the CLI's own duration (`turn_duration`), else the time between the
    /// prompt and the turn's last line. A turn without Claude's answer (a local command) has none.
    fn finish_turn(&mut self, duration: Option<Duration>) {
        let Some(turn) = self.turn.take() else {
            return;
        };
        if !turn.answered && !turn.interrupted {
            return;
        }
        let duration = duration
            .or_else(|| turn.last?.duration_since(turn.started?).ok())
            .unwrap_or_default();
        self.session.restore_entry(EntryKind::TurnEnd(TurnSummary {
            duration,
            cost_usd: 0.,
            is_error: turn.interrupted,
            reason: turn.interrupted.then(|| "aborted_streaming".to_string()),
            errors: Vec::new(),
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_slug_replaces_everything_but_letters_and_digits() {
        assert_eq!(
            project_dir(
                Path::new("/h/.claude/projects"),
                Path::new("/Users/me/dev/my_app")
            ),
            PathBuf::from("/h/.claude/projects/-Users-me-dev-my-app")
        );
    }

    #[test]
    fn prompts_titles_and_wrappers() {
        let user = |content: Value| json!({ "type": "user", "message": { "role": "user", "content": content } });
        assert_eq!(first_prompt(&user(json!("Fix the login"))).as_deref(), Some("Fix the login"));
        assert_eq!(
            first_prompt(&user(json!([{ "type": "text", "text": "  hi  " }]))).as_deref(),
            Some("hi")
        );
        assert_eq!(first_prompt(&user(json!("<command-name>/model</command-name>"))), None);
        assert_eq!(first_prompt(&user(json!("<task-notification>x</task-notification>"))), None);
        assert_eq!(
            first_prompt(&user(json!([{ "type": "tool_result", "tool_use_id": "t", "content": "ok" }]))),
            None
        );
        let mut meta = user(json!("caveat"));
        meta["isMeta"] = json!(true);
        assert_eq!(first_prompt(&meta), None);

        assert_eq!(shorten("one\n  two"), "one two");
        let long = "word ".repeat(40);
        let title = shorten(&long);
        assert!(title.ends_with('…') && title.chars().count() <= TITLE_CHARS);
        assert_eq!(strip_ansi("\u{1b}[1mTotal\u{1b}[22m: $0.10"), "Total: $0.10");
    }
}
