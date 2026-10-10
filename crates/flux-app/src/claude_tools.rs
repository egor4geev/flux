//! Flux's tools for Claude (part 9.2, wiki ADR-030): the `flux` MCP server of every session
//! ([`crate::claude_session`]) offers them, the window runs them. What the IDE knows and the
//! terminal CLI doesn't: the problems the language servers find, definitions, usages, types and
//! symbols from the servers already running, the files open in the editor; and a file to show the
//! user. They only read (and open a tab), so they run without a permission card.
//!
//! After an edit of Claude, [`after_edit`] waits briefly for the file's language server and tells
//! Claude the new errors (Settings → Claude Code → "Tell Claude about new problems…"), as the IDE
//! integrations of Claude Code do.
//!
//! A file nobody has open is opened on its servers in the background (`LspStore::open_background`)
//! for as long as it is among the last few used; the answer waits for the server a few seconds.
//! `open_file` shows the file in a tab but leaves the keyboard where it was (the chat's field).
//!
//! Tool output is for the model: English, plain text, paths relative to the project root, lines and
//! columns 1-based.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use flux_claude::{ToolOutput, ToolSpec};
use flux_core::Rope;
use flux_core::text::{CharClass, char_class, line_start};
use flux_lsp::lsp_types::request::{
    GotoDefinition, HoverRequest, References, WorkspaceSymbolRequest,
};
use flux_lsp::lsp_types::{
    self, GotoDefinitionParams, HoverContents, HoverParams, MarkedString, OneOf,
    ReferenceContext, ReferenceParams, SymbolKind, TextDocumentIdentifier,
    TextDocumentPositionParams, WorkspaceSymbolParams, WorkspaceSymbolResponse,
};
use flux_lsp::{LanguageServer, position};
use futures::channel::oneshot;
use gpui::{App, AsyncApp, Context, Entity, Task, Window};
use serde_json::{Value, json};

use crate::diagnostics::{KnownFile, Severity};
use crate::editor::Editor;
use crate::locations::{self, NavTarget};
use crate::lsp::{FileServers, LspStore};
use crate::navigation::canonical;
use crate::workspace::Workspace;

/// How long a tool waits for a file's language server: to start, to open the file, to report.
const SERVER_WAIT: Duration = Duration::from_secs(5);
/// How long `get_diagnostics` waits for the problems of a file it just opened.
const DIAGNOSTICS_WAIT: Duration = Duration::from_secs(3);
/// How long the edit hook waits for the problems of the new text, then for a second report
/// (rust-analyzer reports its own analysis, then `cargo check`).
const AFTER_EDIT_WAIT: Duration = Duration::from_millis(2500);
const SETTLE_WAIT: Duration = Duration::from_millis(300);
/// How often a tool looks whether the server is there yet.
const POLL: Duration = Duration::from_millis(100);
/// The most lines of problems and places an answer lists.
const MAX_PROBLEMS: usize = 200;
const MAX_PLACES: usize = 100;
const MAX_SYMBOLS: usize = 60;
const MAX_NEW_ERRORS: usize = 20;
/// A line of code in an answer is cut to this many characters.
const MAX_CODE_CHARS: usize = 160;

/// The tools the `flux` server lists.
pub fn specs() -> Vec<ToolSpec> {
    let position = |what: &str| {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "The file, relative to the project root or absolute." },
                "line": { "type": "integer", "description": "The line, 1-based." },
                "symbol": { "type": "string", "description": format!("The name of the symbol on that line {what} (its first occurrence; for `a::b` or `a.b`, the last part).") },
                "column": { "type": "integer", "description": "The column of the symbol, 1-based (instead of `symbol`)." },
            },
            "required": ["path", "line"],
        })
    };
    vec![
        ToolSpec {
            name: "get_diagnostics",
            title: "Get Diagnostics",
            description: "The problems (errors, warnings) Flux's language servers report. Without \
                `path`: every file with problems Flux knows of (the open files and those the servers \
                checked); with `path`: that file, checked by its language server if it isn't yet. \
                Faster than a build to check code you changed.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "A file, relative to the project root or absolute." },
                },
            }),
            read_only: true,
        },
        ToolSpec {
            name: "open_file",
            title: "Open File",
            description: "Opens a file in the user's Flux editor, at a line or selecting a range. \
                Use it to show the user the code you are talking about.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative to the project root or absolute." },
                    "line": { "type": "integer", "description": "1-based." },
                    "column": { "type": "integer", "description": "1-based." },
                    "end_line": { "type": "integer", "description": "Select up to this line, 1-based." },
                    "end_column": { "type": "integer", "description": "1-based, exclusive." },
                },
                "required": ["path"],
            }),
            read_only: true,
        },
        ToolSpec {
            name: "get_open_files",
            title: "Get Open Files",
            description: "The files open in the user's Flux editor, the active one first: whether \
                they have unsaved changes, the caret and the selection.",
            input_schema: json!({ "type": "object", "properties": {} }),
            read_only: true,
        },
        ToolSpec {
            name: "find_definition",
            title: "Find Definition",
            description: "Where a symbol is defined, from the language server: precise where grep \
                guesses (same names, re-exports, generated code, dependencies).",
            input_schema: position("to look up"),
            read_only: true,
        },
        ToolSpec {
            name: "find_usages",
            title: "Find Usages",
            description: "Every place a symbol is used in the project, from the language server \
                (the declaration itself is not listed).",
            input_schema: position("to find"),
            read_only: true,
        },
        ToolSpec {
            name: "symbol_info",
            title: "Symbol Info",
            description: "The type, the signature and the documentation of a symbol, from the \
                language server.",
            input_schema: position("to describe"),
            read_only: true,
        },
        ToolSpec {
            name: "search_symbols",
            title: "Search Symbols",
            description: "Types, functions and other symbols of the project whose names match a \
                query, from the running language servers.",
            input_schema: json!({
                "type": "object",
                "properties": { "query": { "type": "string" } },
                "required": ["query"],
            }),
            read_only: true,
        },
    ]
}

/// Runs a tool Claude called ([`crate::claude::ClaudeStoreEvent::ToolCall`]).
pub fn call(
    workspace: &mut Workspace,
    tool: &str,
    input: &Value,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Task<ToolOutput> {
    let result = match tool {
        "get_diagnostics" => get_diagnostics(workspace, input, cx),
        "open_file" => open_file(workspace, input, window, cx),
        "get_open_files" => Ok(Task::ready(ToolOutput::text(get_open_files(workspace, cx)))),
        "find_definition" => navigate(workspace, Query::Definition, input, cx),
        "find_usages" => navigate(workspace, Query::Usages, input, cx),
        "symbol_info" => navigate(workspace, Query::Info, input, cx),
        "search_symbols" => search_symbols(workspace, input, cx),
        tool => Err(format!("Flux has no tool {tool}.")),
    };
    result.unwrap_or_else(|error| Task::ready(ToolOutput::error(error)))
}

// --- Paths and positions ---

/// The file a tool names: relative to the project root or absolute; it must exist.
fn resolve_path(root: Option<&Path>, input: &Value) -> Result<PathBuf, String> {
    let Some(given) = input["path"].as_str().filter(|path| !path.trim().is_empty()) else {
        return Err("`path` is required.".into());
    };
    let path = Path::new(given.trim());
    let path = match root {
        Some(root) if path.is_relative() => root.join(path),
        _ => path.to_path_buf(),
    };
    if path.is_dir() {
        return Err(format!("{given} is a folder, not a file."));
    }
    if !path.is_file() {
        return Err(format!("There is no file {given}."));
    }
    Ok(normalized(&path))
}

/// `a/./b/../c` → `a/c`, without touching the disk (symlinks stay as the user sees them).
fn normalized(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            part => out.push(part),
        }
    }
    out
}

/// A path for the model: relative to the root inside the project, else absolute.
fn shown(path: &Path, root: Option<&Path>) -> String {
    root.and_then(|root| path.strip_prefix(root).ok())
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// 1-based line and column of a character index.
fn line_col(text: &Rope, pos: usize) -> (usize, usize) {
    let pos = pos.min(text.len_chars());
    let line = text.char_to_line(pos);
    (line + 1, pos - line_start(text, line) + 1)
}

/// The character index of `line` (1-based) and `column` or `symbol` on it.
fn resolve_position(text: &Rope, input: &Value) -> Result<usize, String> {
    let lines = text.len_lines();
    let Some(line) = input["line"].as_u64().map(|line| line as usize) else {
        return Err("`line` is required (1-based).".into());
    };
    if line == 0 || line > lines {
        return Err(format!("Line {line} is out of range: the file has {lines} lines."));
    }
    let start = line_start(text, line - 1);
    let content: String = text
        .line(line - 1)
        .chars()
        .take_while(|c| *c != '\n' && *c != '\r')
        .collect();
    if let Some(column) = input["column"].as_u64().map(|column| column as usize) {
        let length = content.chars().count();
        if column == 0 || column > length + 1 {
            return Err(format!(
                "Column {column} is out of range: line {line} has {length} characters."
            ));
        }
        return Ok(start + column - 1);
    }
    let Some(symbol) = input["symbol"].as_str().map(str::trim).filter(|s| !s.is_empty()) else {
        return Err("Give `symbol` (the name on that line) or `column`.".into());
    };
    match find_symbol(&content, symbol) {
        Some(column) => Ok(start + column),
        None => Err(format!(
            "`{symbol}` is not on line {line}: `{}`",
            cut(content.trim(), MAX_CODE_CHARS)
        )),
    }
}

/// Where `symbol` is on a line, as a whole word (its last part for `a::b`, `a.b`): the character
/// index of that part.
fn find_symbol(line: &str, symbol: &str) -> Option<usize> {
    let chars: Vec<char> = line.chars().collect();
    let wanted: Vec<char> = symbol.chars().collect();
    // The last identifier of a path: `std::fs::read` → `read`.
    let last = symbol
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map_or(0, |at| symbol[..=at].chars().count());
    let is_word = |c: char| char_class(c) == CharClass::Word;
    (0..chars.len())
        .filter(|&at| chars[at..].starts_with(&wanted))
        .find(|&at| {
            let before = at.checked_sub(1).map(|i| chars[i]);
            let after = chars.get(at + wanted.len()).copied();
            !before.is_some_and(is_word) && !after.is_some_and(is_word)
        })
        .map(|at| at + last)
}

/// Text for one line of an answer: at most `max` characters.
fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max - 1).collect();
    format!("{kept}…")
}

/// The text of an open document (with its unsaved changes), by canonical path.
fn open_texts(workspace: &Workspace, cx: &App) -> HashMap<PathBuf, Rope> {
    workspace
        .editors(cx)
        .into_iter()
        .filter_map(|editor| {
            let editor = editor.read(cx);
            Some((
                canonical(editor.document.path()?),
                editor.document.text().clone(),
            ))
        })
        .collect()
}

fn editor_for(workspace: &Workspace, path: &Path, cx: &App) -> Option<Entity<Editor>> {
    let key = canonical(path);
    workspace
        .editors(cx)
        .into_iter()
        .find(|editor| editor.read(cx).document.path().map(canonical) == Some(key.clone()))
}

/// Waits for `receiver` at most `timeout`: `true` — it came.
async fn wait(receiver: oneshot::Receiver<()>, timeout: Duration, cx: &mut AsyncApp) -> bool {
    let timer = cx.background_executor().timer(timeout);
    matches!(
        futures::future::select(receiver, timer).await,
        futures::future::Either::Left((Ok(()), _))
    )
}

/// Why a file's server can't answer, for the model.
fn server_problem(lsp: &LspStore, path: &Path, shown_path: &str) -> String {
    match lsp.file_waiting(path) {
        Some(FileServers::Starting(name)) => {
            format!("{name} is still starting for {shown_path}; try again in a moment.")
        }
        Some(FileServers::Missing(name)) => format!("{name} is not installed, so Flux can't answer."),
        Some(FileServers::Failed(name, reason)) => format!("{name} stopped: {reason}"),
        None if !lsp.serves(path) => {
            format!("Flux has no language server for {shown_path}.")
        }
        None => format!("The language server hasn't opened {shown_path} yet; try again."),
    }
}

/// The main server of `path` with the file open on it: an open editor's, or the file opened in the
/// background, waiting up to [`SERVER_WAIT`] for the server to start.
/// A file's main server and the file's URI on it, or why there is none.
type FileServer = Result<(LanguageServer, lsp_types::Uri), String>;

fn file_server(
    workspace: &mut Workspace,
    path: &Path,
    cx: &mut Context<Workspace>,
) -> Result<Task<FileServer>, String> {
    let lsp = workspace.lsp.clone();
    let root = workspace.root().map(Path::to_path_buf);
    let shown_path = shown(path, root.as_deref());
    if !lsp.read(cx).serves(path) {
        return Err(format!("Flux has no language server for {shown_path}."));
    }
    if let Some(found) = lsp.read(cx).file_server(path, cx) {
        return Ok(Task::ready(Ok(found)));
    }
    if editor_for(workspace, path, cx).is_none()
        && !lsp.update(cx, |lsp, cx| lsp.open_background(path, cx))
    {
        return Err(format!("Flux can't open {shown_path} on its language server (too big?)."));
    }
    let path = path.to_path_buf();
    Ok(cx.spawn(async move |_, cx| {
        let started = std::time::Instant::now();
        loop {
            let found = lsp.read_with(cx, |lsp, cx| lsp.file_server(&path, cx));
            match found {
                Ok(Some(found)) => return Ok(found),
                Ok(None) => {}
                Err(_) => return Err("The window is closing.".to_string()),
            }
            let gave_up = lsp
                .read_with(cx, |lsp, _| {
                    matches!(
                        lsp.file_waiting(&path),
                        Some(FileServers::Missing(_) | FileServers::Failed(..))
                    )
                })
                .unwrap_or(true);
            if gave_up || started.elapsed() > SERVER_WAIT {
                return Err(lsp
                    .read_with(cx, |lsp, _| server_problem(lsp, &path, &shown_path))
                    .unwrap_or_default());
            }
            cx.background_executor().timer(POLL).await;
        }
    }))
}

// --- get_diagnostics ---

/// One problem for the model.
#[derive(Debug, Clone, PartialEq)]
struct Problem {
    line: usize,
    column: usize,
    severity: Severity,
    message: String,
    origin: Option<String>,
}

impl Problem {
    fn from_lsp(diagnostic: &lsp_types::Diagnostic) -> Problem {
        Problem {
            line: diagnostic.range.start.line as usize + 1,
            column: diagnostic.range.start.character as usize + 1,
            severity: match diagnostic.severity {
                Some(lsp_types::DiagnosticSeverity::WARNING) => Severity::Warning,
                Some(lsp_types::DiagnosticSeverity::INFORMATION) => Severity::Info,
                Some(lsp_types::DiagnosticSeverity::HINT) => Severity::Hint,
                _ => Severity::Error,
            },
            message: diagnostic.message.clone(),
            origin: origin(
                diagnostic.source.as_deref(),
                diagnostic.code.as_ref().map(|code| match code {
                    lsp_types::NumberOrString::Number(n) => n.to_string(),
                    lsp_types::NumberOrString::String(s) => s.clone(),
                }),
            ),
        }
    }

    fn line(&self) -> String {
        let severity = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
            Severity::Hint => "hint",
        };
        let message = self.message.lines().collect::<Vec<_>>().join(" ");
        match &self.origin {
            Some(origin) => format!(
                "{}:{} {severity}: {} [{origin}]",
                self.line,
                self.column,
                cut(&message, 400)
            ),
            None => format!("{}:{} {severity}: {}", self.line, self.column, cut(&message, 400)),
        }
    }
}

fn origin(source: Option<&str>, code: Option<String>) -> Option<String> {
    let origin = [source.map(str::to_string), code]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    (!origin.is_empty()).then_some(origin)
}

/// The problems of an open document, hints left out.
fn editor_problems(editor: &Editor) -> Vec<Problem> {
    let text = editor.document.text();
    editor
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity != Severity::Hint)
        .map(|diagnostic| {
            let (line, column) = line_col(text, diagnostic.range.start);
            Problem {
                line,
                column,
                severity: diagnostic.severity,
                message: diagnostic.message.clone(),
                origin: origin(diagnostic.source.as_deref(), diagnostic.code.clone()),
            }
        })
        .collect()
}

/// The problems known for every file: open documents, then the files only the servers checked.
fn all_problems(workspace: &Workspace, cx: &App) -> Vec<(PathBuf, Vec<Problem>)> {
    let mut files: Vec<(PathBuf, Vec<Problem>)> = Vec::new();
    for (path, known) in crate::diagnostics::known_files(workspace, cx) {
        let problems: Vec<Problem> = match known {
            KnownFile::Open(editor) => editor_problems(editor.read(cx)),
            KnownFile::Published(diagnostics) => diagnostics
                .iter()
                .map(Problem::from_lsp)
                .filter(|problem| problem.severity != Severity::Hint)
                .collect(),
        };
        if !problems.is_empty() {
            files.push((path, problems));
        }
    }
    for (_, problems) in &mut files {
        problems.sort_by_key(|problem| (problem.severity, problem.line, problem.column));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

/// Problems of files, for the model: a heading per file with its counts, a line per problem.
fn format_problems(files: &[(PathBuf, Vec<Problem>)], root: Option<&Path>) -> String {
    let mut out = String::new();
    let mut listed = 0;
    let total: usize = files.iter().map(|(_, problems)| problems.len()).sum();
    for (path, problems) in files {
        let errors = problems.iter().filter(|p| p.severity == Severity::Error).count();
        let warnings = problems.iter().filter(|p| p.severity == Severity::Warning).count();
        let _ = writeln!(
            out,
            "{} — {}",
            shown(path, root),
            counts(errors, warnings, problems.len() - errors - warnings)
        );
        for problem in problems {
            if listed == MAX_PROBLEMS {
                let _ = writeln!(out, "… {} more", total - listed);
                return out;
            }
            let _ = writeln!(out, "  {}", problem.line());
            listed += 1;
        }
    }
    out
}

fn counts(errors: usize, warnings: usize, other: usize) -> String {
    let plural = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let mut parts = Vec::new();
    if errors > 0 {
        parts.push(plural(errors, "error", "errors"));
    }
    if warnings > 0 {
        parts.push(plural(warnings, "warning", "warnings"));
    }
    if other > 0 {
        parts.push(plural(other, "note", "notes"));
    }
    parts.join(", ")
}

fn get_diagnostics(
    workspace: &mut Workspace,
    input: &Value,
    cx: &mut Context<Workspace>,
) -> Result<Task<ToolOutput>, String> {
    let root = workspace.root().map(Path::to_path_buf);
    if input["path"].as_str().is_none_or(|path| path.trim().is_empty()) {
        let files = all_problems(workspace, cx);
        let running = workspace.lsp.read(cx).running_servers();
        let text = if files.is_empty() {
            match running.is_empty() {
                true => "No language server is running yet, so there are no problems to report. \
                    Ask for a file (`path`) to have it checked."
                    .to_string(),
                false => "No problems in the files the language servers have checked.".to_string(),
            }
        } else {
            format_problems(&files, root.as_deref())
        };
        return Ok(Task::ready(ToolOutput::text(text)));
    }
    let path = resolve_path(root.as_deref(), input)?;
    let shown_path = shown(&path, root.as_deref());
    // An open document: the editor has the problems of its text (unsaved changes included).
    if let Some(editor) = editor_for(workspace, &path, cx) {
        let lsp = workspace.lsp.read(cx);
        if lsp.file_server(&path, cx).is_none() {
            return Err(server_problem(lsp, &path, &shown_path));
        }
        let problems = editor_problems(editor.read(cx));
        return Ok(Task::ready(ToolOutput::text(one_file(&shown_path, &path, problems, root.as_deref()))));
    }
    let lsp = workspace.lsp.clone();
    let opened_before = lsp.read(cx).file_server(&path, cx).is_some();
    let known = || {
        lsp.read(cx)
            .unopened_diagnostics()
            .into_iter()
            .filter(|(known, ..)| *known == path)
            .flat_map(|(_, _, diagnostics)| diagnostics)
            .collect::<Vec<_>>()
    };
    if opened_before {
        let problems = known().iter().map(Problem::from_lsp).collect();
        return Ok(Task::ready(ToolOutput::text(one_file(&shown_path, &path, problems, root.as_deref()))));
    }
    let waiter = lsp.update(cx, |lsp, _| lsp.wait_diagnostics(&path));
    let server = file_server(workspace, &path, cx)?;
    Ok(cx.spawn(async move |_, cx| {
        if let Err(error) = server.await {
            return ToolOutput::error(error);
        }
        let reported = wait(waiter, DIAGNOSTICS_WAIT, cx).await;
        let problems: Vec<Problem> = lsp
            .read_with(cx, |lsp, _| {
                lsp.unopened_diagnostics()
                    .into_iter()
                    .filter(|(known, ..)| *known == path)
                    .flat_map(|(_, _, diagnostics)| diagnostics)
                    .map(|diagnostic| Problem::from_lsp(&diagnostic))
                    .filter(|problem| problem.severity != Severity::Hint)
                    .collect()
            })
            .unwrap_or_default();
        if !reported && problems.is_empty() {
            return ToolOutput::text(format!(
                "The language server hasn't reported on {shown_path} yet (it may still be \
                 indexing the project); try again shortly."
            ));
        }
        ToolOutput::text(one_file(&shown_path, &path, problems, root.as_deref()))
    }))
}

fn one_file(shown_path: &str, path: &Path, problems: Vec<Problem>, root: Option<&Path>) -> String {
    let mut problems: Vec<Problem> = problems
        .into_iter()
        .filter(|problem| problem.severity != Severity::Hint)
        .collect();
    if problems.is_empty() {
        return format!("No problems in {shown_path}.");
    }
    problems.sort_by_key(|problem| (problem.severity, problem.line, problem.column));
    format_problems(&[(path.to_path_buf(), problems)], root)
}

// --- open_file ---

fn open_file(
    workspace: &mut Workspace,
    input: &Value,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Result<Task<ToolOutput>, String> {
    let root = workspace.root().map(Path::to_path_buf);
    let path = resolve_path(root.as_deref(), input)?;
    let number = |key: &str| {
        input[key]
            .as_u64()
            .map(|n| (n as usize).max(1))
    };
    let line = number("line");
    let column = number("column").unwrap_or(1);
    let end = number("end_line").map(|end_line| (end_line, number("end_column")));
    let shown_path = shown(&path, root.as_deref());
    let text = match (line, end) {
        (None, _) => format!("Opened {shown_path} in the editor."),
        (Some(line), None) => format!("Opened {shown_path} at line {line} in the editor."),
        (Some(line), Some((end_line, _))) => {
            format!("Opened {shown_path} in the editor, lines {line}–{end_line} selected.")
        }
    };
    // The keyboard stays where it was (the chat's field): the user reads the file while answering.
    workspace.open_and(path, false, window, cx, move |workspace, cx| {
        let (Some(line), Some(editor)) = (line, workspace.active_editor()) else {
            return;
        };
        editor.update(cx, |editor, cx| {
            let start = editor.position(line - 1, column - 1);
            let end = match end {
                Some((end_line, end_column)) => {
                    editor.position(end_line - 1, end_column.map_or(usize::MAX, |c| c - 1))
                }
                None => start,
            };
            editor.select_range(start..end.max(start), cx);
        });
    });
    Ok(Task::ready(ToolOutput::text(text)))
}

// --- get_open_files ---

fn get_open_files(workspace: &Workspace, cx: &App) -> String {
    let root = workspace.root();
    let active = workspace.active_editor();
    let mut editors = workspace.editors(cx);
    // The active one first.
    if let Some(active) = &active
        && let Some(at) = editors.iter().position(|editor| editor == active)
    {
        let editor = editors.remove(at);
        editors.insert(0, editor);
    }
    if editors.is_empty() {
        return "No files are open in the editor.".into();
    }
    let mut out = String::from("Files open in the editor (the active one first):\n");
    for editor in &editors {
        let is_active = active.as_ref() == Some(editor);
        let editor = editor.read(cx);
        let document = &editor.document;
        let name = document
            .path()
            .map_or_else(|| document.display_name(), |path| shown(path, root));
        let text = document.text();
        let selection = document.selection().primary();
        let mut notes = Vec::new();
        if is_active {
            notes.push("active".to_string());
        }
        if document.is_modified() {
            notes.push("unsaved changes".to_string());
        }
        let (line, column) = line_col(text, selection.head);
        let mut place = format!("caret {line}:{column}");
        if selection.from() != selection.to() {
            let (from_line, from_column) = line_col(text, selection.from());
            let (to_line, to_column) = line_col(text, selection.to());
            let _ = write!(place, ", selection {from_line}:{from_column}–{to_line}:{to_column}");
        }
        let notes = if notes.is_empty() {
            String::new()
        } else {
            format!(" ({})", notes.join(", "))
        };
        let _ = writeln!(out, "- {name}{notes} — {place}");
    }
    out
}

// --- Navigation ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Query {
    Definition,
    Usages,
    Info,
}

fn navigate(
    workspace: &mut Workspace,
    query: Query,
    input: &Value,
    cx: &mut Context<Workspace>,
) -> Result<Task<ToolOutput>, String> {
    let root = workspace.root().map(Path::to_path_buf);
    let path = resolve_path(root.as_deref(), input)?;
    // The text the server has: the editor's, or the disk's for a background copy.
    let text = match editor_for(workspace, &path, cx) {
        Some(editor) => editor.read(cx).document.text().clone(),
        None => Rope::from_str(
            &std::fs::read_to_string(&path).map_err(|error| format!("Can't read the file: {error}"))?,
        ),
    };
    let at = resolve_position(&text, input)?;
    let lsp_position = position::to_lsp(&text, at);
    let server = file_server(workspace, &path, cx)?;
    let texts = open_texts(workspace, cx);
    let symbol = word_at(&text, at);
    let (line, column) = line_col(&text, at);
    let shown_path = shown(&path, root.as_deref());
    Ok(cx.spawn(async move |_, cx| {
        let (server, uri) = match server.await {
            Ok(found) => found,
            Err(error) => return ToolOutput::error(error),
        };
        let document = TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: lsp_position,
        };
        let name = symbol.clone().unwrap_or_else(|| format!("{shown_path}:{line}:{column}"));
        let targets = match query {
            Query::Info => {
                let response = server
                    .request::<HoverRequest>(HoverParams {
                        text_document_position_params: document,
                        work_done_progress_params: Default::default(),
                    })
                    .await;
                return match response {
                    Ok(Some(hover)) => match hover_text(hover.contents) {
                        Some(info) => ToolOutput::text(format!(
                            "`{name}` at {shown_path}:{line}:{column}:\n\n{info}"
                        )),
                        None => ToolOutput::text(format!("The language server knows nothing about `{name}` there.")),
                    },
                    Ok(None) => ToolOutput::text(format!(
                        "The language server knows nothing about `{name}` at {shown_path}:{line}:{column}."
                    )),
                    Err(error) => ToolOutput::error(format!("{}: {error}", server.name())),
                };
            }
            Query::Definition => server
                .request::<GotoDefinition>(GotoDefinitionParams {
                    text_document_position_params: document,
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                })
                .await
                .map(locations::from_definition),
            Query::Usages => server
                .request::<References>(ReferenceParams {
                    text_document_position: document,
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                    context: ReferenceContext {
                        include_declaration: false,
                    },
                })
                .await
                .map(|locations| locations::from_locations(locations.unwrap_or_default())),
        };
        let targets = match targets {
            Ok(targets) => targets,
            Err(error) => return ToolOutput::error(format!("{}: {error}", server.name())),
        };
        if targets.is_empty() {
            return ToolOutput::text(match query {
                Query::Definition => format!("The language server found no definition of `{name}`."),
                _ => format!("The language server found no usages of `{name}`."),
            });
        }
        let what = match query {
            Query::Definition if targets.len() == 1 => format!("The definition of `{name}`:"),
            Query::Definition => format!("{} definitions of `{name}`:", targets.len()),
            _ if targets.len() == 1 => format!("1 usage of `{name}`:"),
            _ => format!("{} usages of `{name}`:", targets.len()),
        };
        let places = cx
            .background_executor()
            .spawn(async move { format_places(&targets, &texts, root.as_deref()) })
            .await;
        ToolOutput::text(format!("{what}\n{places}"))
    }))
}

/// The word under a position (the symbol's name for the answer).
fn word_at(text: &Rope, pos: usize) -> Option<String> {
    crate::navigation::word_at(text, pos).map(|range| text.slice(range).to_string())
}

/// Places with their code: `path:line:column  code`, ordered by file and position; open
/// documents' text from `texts`, others read from disk.
fn format_places(targets: &[NavTarget], texts: &HashMap<PathBuf, Rope>, root: Option<&Path>) -> String {
    let mut sorted: Vec<&NavTarget> = targets.iter().collect();
    sorted.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then_with(|| a.range.start.line.cmp(&b.range.start.line))
            .then_with(|| a.range.start.character.cmp(&b.range.start.character))
    });
    let mut read: HashMap<PathBuf, Option<Rope>> = HashMap::new();
    let mut out = String::new();
    for (index, target) in sorted.iter().enumerate() {
        if index == MAX_PLACES {
            let _ = writeln!(out, "… {} more", sorted.len() - index);
            break;
        }
        let key = canonical(&target.path);
        let text = match texts.get(&key) {
            Some(text) => Some(text.clone()),
            None => read
                .entry(key)
                .or_insert_with(|| {
                    std::fs::read_to_string(&target.path)
                        .ok()
                        .map(|text| Rope::from_str(&text))
                })
                .clone(),
        };
        let line = target.range.start.line as usize;
        let code = text
            .filter(|text| line < text.len_lines())
            .map(|text| {
                let content: String = text
                    .line(line)
                    .chars()
                    .take_while(|c| *c != '\n' && *c != '\r')
                    .collect();
                cut(content.trim(), MAX_CODE_CHARS)
            })
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "{}:{}:{}  {code}",
            shown(&target.path, root),
            line + 1,
            target.range.start.character + 1
        );
    }
    out
}

/// Hover contents as Markdown for the model.
fn hover_text(contents: HoverContents) -> Option<String> {
    let marked = |marked: MarkedString| match marked {
        MarkedString::String(text) => text,
        MarkedString::LanguageString(code) => {
            format!("```{}\n{}\n```", code.language, code.value)
        }
    };
    let text = match contents {
        HoverContents::Scalar(scalar) => marked(scalar),
        HoverContents::Array(items) => items.into_iter().map(marked).collect::<Vec<_>>().join("\n\n"),
        HoverContents::Markup(markup) => markup.value,
    };
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

// --- search_symbols ---

fn search_symbols(
    workspace: &mut Workspace,
    input: &Value,
    cx: &mut Context<Workspace>,
) -> Result<Task<ToolOutput>, String> {
    let Some(query) = input["query"].as_str().map(str::trim).filter(|q| !q.is_empty()) else {
        return Err("`query` is required.".into());
    };
    let query = query.to_string();
    let root = workspace.root().map(Path::to_path_buf);
    let servers = workspace.lsp.read(cx).running_servers();
    let servers: Vec<LanguageServer> = servers
        .into_iter()
        .filter(|server| {
            server.capabilities().is_some_and(|capabilities| {
                matches!(
                    capabilities.workspace_symbol_provider,
                    Some(OneOf::Left(true) | OneOf::Right(_))
                )
            })
        })
        .collect();
    if servers.is_empty() {
        return Err("No language server that searches symbols is running yet: open a file of the \
            project (or ask `get_diagnostics` for one) to start its server."
            .into());
    }
    Ok(cx.spawn(async move |_, _| {
        let requests: Vec<_> = servers
            .iter()
            .map(|server| {
                server.request::<WorkspaceSymbolRequest>(WorkspaceSymbolParams {
                    query: query.clone(),
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                })
            })
            .collect();
        let answers = futures::future::join_all(requests).await;
        let mut symbols: Vec<(String, SymbolKind, Option<String>, PathBuf, u32)> = Vec::new();
        for answer in answers.into_iter().flatten().flatten() {
            match answer {
                WorkspaceSymbolResponse::Flat(items) => {
                    for item in items {
                        if let Some(path) = position::path_from_uri(&item.location.uri) {
                            symbols.push((
                                item.name,
                                item.kind,
                                item.container_name,
                                path,
                                item.location.range.start.line,
                            ));
                        }
                    }
                }
                WorkspaceSymbolResponse::Nested(items) => {
                    for item in items {
                        let (uri, line) = match item.location {
                            OneOf::Left(location) => (location.uri, location.range.start.line),
                            OneOf::Right(location) => (location.uri, 0),
                        };
                        if let Some(path) = position::path_from_uri(&uri) {
                            symbols.push((item.name, item.kind, item.container_name, path, line));
                        }
                    }
                }
            }
        }
        if symbols.is_empty() {
            return ToolOutput::text(format!("No symbols match `{query}`."));
        }
        // Inside the project first.
        symbols.sort_by_key(|(name, _, _, path, line)| {
            let outside = root.as_deref().is_none_or(|root| !path.starts_with(root));
            (outside, name.len(), path.clone(), *line)
        });
        let total = symbols.len();
        let mut out = format!(
            "{total} symbol{} matching `{query}`:\n",
            if total == 1 { "" } else { "s" }
        );
        for (name, kind, container, path, line) in symbols.into_iter().take(MAX_SYMBOLS) {
            let container = container
                .filter(|container| !container.is_empty())
                .map(|container| format!(" in {container}"))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "- {name} ({}){container} — {}:{}",
                kind_name(kind),
                shown(&path, root.as_deref()),
                line + 1
            );
        }
        if total > MAX_SYMBOLS {
            let _ = writeln!(out, "… {} more", total - MAX_SYMBOLS);
        }
        ToolOutput::text(out)
    }))
}

fn kind_name(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::FILE => "file",
        SymbolKind::MODULE => "module",
        SymbolKind::NAMESPACE => "namespace",
        SymbolKind::PACKAGE => "package",
        SymbolKind::CLASS => "class",
        SymbolKind::METHOD => "method",
        SymbolKind::PROPERTY => "property",
        SymbolKind::FIELD => "field",
        SymbolKind::CONSTRUCTOR => "constructor",
        SymbolKind::ENUM => "enum",
        SymbolKind::INTERFACE => "interface",
        SymbolKind::FUNCTION => "function",
        SymbolKind::VARIABLE => "variable",
        SymbolKind::CONSTANT => "constant",
        SymbolKind::STRING => "string",
        SymbolKind::NUMBER => "number",
        SymbolKind::BOOLEAN => "boolean",
        SymbolKind::ARRAY => "array",
        SymbolKind::OBJECT => "object",
        SymbolKind::KEY => "key",
        SymbolKind::NULL => "null",
        SymbolKind::ENUM_MEMBER => "enum member",
        SymbolKind::STRUCT => "struct",
        SymbolKind::EVENT => "event",
        SymbolKind::OPERATOR => "operator",
        SymbolKind::TYPE_PARAMETER => "type parameter",
        _ => "symbol",
    }
}

// --- After an edit ---

/// After an edit of Claude to `path`: the errors its language server now reports there that weren't
/// there before, as a note for Claude; `None` — none, no server, the document has unsaved changes of
/// the user, or the setting is off.
pub fn after_edit(
    workspace: &mut Workspace,
    path: &Path,
    cx: &mut Context<Workspace>,
) -> Task<Option<String>> {
    if !crate::settings::claude(cx).report_problems {
        return Task::ready(None);
    }
    let lsp = workspace.lsp.clone();
    if !lsp.read(cx).serves(path) {
        return Task::ready(None);
    }
    let path = normalized(path);
    let root = workspace.root().map(Path::to_path_buf);
    let editor = editor_for(workspace, &path, cx);
    // The errors of the text before the edit.
    let before: Vec<Problem> = match &editor {
        Some(editor) => editor_problems(editor.read(cx)),
        None => known_problems(&lsp, &path, cx),
    }
    .into_iter()
    .filter(|problem| problem.severity == Severity::Error)
    .collect();
    // The new text reaches the servers: the open document takes it now (as `sync_documents`
    // would a moment later), a background copy is told, an unopened file is opened.
    match &editor {
        Some(editor) => {
            if editor.read(cx).document.is_modified() {
                return Task::ready(None);
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                return Task::ready(None);
            };
            let mtime = crate::editor::file_mtime(&path);
            editor.update(cx, |editor, cx| {
                if editor.document.text() != content.as_str() {
                    editor.reload(&content, cx);
                }
                editor.disk_mtime = mtime;
            });
            if lsp.read(cx).file_server(&path, cx).is_none() {
                return Task::ready(None);
            }
        }
        None => {
            let refreshed = lsp.update(cx, |lsp, _| lsp.refresh_background(&path));
            if !refreshed && !lsp.update(cx, |lsp, cx| lsp.open_background(&path, cx)) {
                return Task::ready(None);
            }
        }
    }
    let waiter = lsp.update(cx, |lsp, _| lsp.wait_diagnostics(&path));
    cx.spawn(async move |this, cx| {
        if !wait(waiter, AFTER_EDIT_WAIT, cx).await {
            return None;
        }
        // A second report may follow at once (rust-analyzer: its analysis, then `cargo check`).
        let second = lsp
            .update(cx, |lsp, _| lsp.wait_diagnostics(&path))
            .ok()?;
        wait(second, SETTLE_WAIT, cx).await;
        let after: Vec<Problem> = this
            .update(cx, |workspace, cx| match editor_for(workspace, &path, cx) {
                Some(editor) => editor_problems(editor.read(cx)),
                None => known_problems(&lsp, &path, cx),
            })
            .ok()?
            .into_iter()
            .filter(|problem| problem.severity == Severity::Error)
            .collect();
        new_errors_note(&shown(&path, root.as_deref()), &before, &after)
    })
}

/// The problems the servers published for a file no editor has open.
fn known_problems(lsp: &Entity<LspStore>, path: &Path, cx: &App) -> Vec<Problem> {
    lsp.read(cx)
        .unopened_diagnostics()
        .into_iter()
        .filter(|(known, ..)| known == path)
        .flat_map(|(_, _, diagnostics)| diagnostics)
        .map(|diagnostic| Problem::from_lsp(&diagnostic))
        .collect()
}

/// The note for Claude: the errors of `after` that `before` didn't have (by message and origin,
/// counted — positions move with the edit); `None` without new ones.
fn new_errors_note(shown_path: &str, before: &[Problem], after: &[Problem]) -> Option<String> {
    let mut old: Vec<(&str, Option<&str>)> = before
        .iter()
        .map(|problem| (problem.message.as_str(), problem.origin.as_deref()))
        .collect();
    let mut new = Vec::new();
    for problem in after {
        let key = (problem.message.as_str(), problem.origin.as_deref());
        match old.iter().position(|known| *known == key) {
            Some(at) => {
                old.remove(at);
            }
            None => new.push(problem),
        }
    }
    if new.is_empty() {
        return None;
    }
    let count = new.len();
    let mut note = format!(
        "Flux: the language server now reports {count} new error{} in {shown_path} after this edit:\n",
        if count == 1 { "" } else { "s" }
    );
    for problem in new.iter().take(MAX_NEW_ERRORS) {
        let _ = writeln!(note, "- {}", problem.line());
    }
    if count > MAX_NEW_ERRORS {
        let _ = writeln!(note, "… {} more", count - MAX_NEW_ERRORS);
    }
    let older = after.len() - count;
    if older > 0 {
        let _ = writeln!(
            note,
            "({older} other error{} in the file {} there before the edit.)",
            if older == 1 { "" } else { "s" },
            if older == 1 { "was" } else { "were" }
        );
    }
    Some(note)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rope(text: &str) -> Rope {
        Rope::from_str(text)
    }

    #[test]
    fn positions_come_from_a_column_or_a_symbol_on_the_line() {
        let text = rope("fn main() {\n    let total = sum(&items);\n}\n");
        let at = |input: Value| resolve_position(&text, &input);
        assert_eq!(at(json!({ "line": 2, "symbol": "sum" })), Ok(12 + 16));
        assert_eq!(at(json!({ "line": 2, "column": 9 })), Ok(12 + 8));
        // Whole words only: `total` isn't found inside `subtotal`.
        let words = rope("let subtotal = total;\n");
        assert_eq!(resolve_position(&words, &json!({ "line": 1, "symbol": "total" })), Ok(15));
        // The last part of a path.
        let path = rope("use std::fs::read;\n");
        assert_eq!(
            resolve_position(&path, &json!({ "line": 1, "symbol": "fs::read" })),
            Ok(13)
        );
        assert_eq!(
            at(json!({ "line": 2, "symbol": "missing" })),
            Err("`missing` is not on line 2: `let total = sum(&items);`".into())
        );
        assert!(at(json!({ "line": 9, "symbol": "sum" })).unwrap_err().contains("out of range"));
        assert!(at(json!({ "line": 2 })).unwrap_err().contains("`symbol`"));
        assert!(at(json!({ "line": 2, "column": 99 })).unwrap_err().contains("out of range"));
    }

    #[test]
    fn paths_are_relative_to_the_root_and_must_exist() {
        let dir = std::env::temp_dir().join(format!("flux-claude-tools-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/lib.rs"), "").unwrap();
        let input = |path: &str| json!({ "path": path });
        assert_eq!(
            resolve_path(Some(&dir), &input("src/lib.rs")),
            Ok(dir.join("src/lib.rs"))
        );
        assert_eq!(
            resolve_path(Some(&dir), &input("./src/../src/lib.rs")),
            Ok(dir.join("src/lib.rs"))
        );
        assert_eq!(
            resolve_path(Some(&dir), &input("src/nope.rs")),
            Err("There is no file src/nope.rs.".into())
        );
        assert!(resolve_path(Some(&dir), &input("src")).unwrap_err().contains("folder"));
        assert_eq!(shown(&dir.join("src/lib.rs"), Some(&dir)), "src/lib.rs");
        assert_eq!(shown(Path::new("/elsewhere/a.rs"), Some(&dir)), "/elsewhere/a.rs");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn problem(line: usize, severity: Severity, message: &str) -> Problem {
        Problem {
            line,
            column: 5,
            severity,
            message: message.into(),
            origin: Some("rustc E0308".into()),
        }
    }

    #[test]
    fn problems_read_as_lines_per_file() {
        let root = Path::new("/p");
        let files = vec![(
            PathBuf::from("/p/src/main.rs"),
            vec![
                problem(3, Severity::Error, "mismatched types\nexpected `u32`"),
                problem(9, Severity::Warning, "unused variable"),
            ],
        )];
        assert_eq!(
            format_problems(&files, Some(root)),
            "src/main.rs — 1 error, 1 warning\n  3:5 error: mismatched types expected `u32` \
             [rustc E0308]\n  9:5 warning: unused variable [rustc E0308]\n"
        );
    }

    #[test]
    fn only_new_errors_are_told() {
        let before = vec![problem(3, Severity::Error, "mismatched types")];
        let after = vec![
            problem(4, Severity::Error, "mismatched types"),
            problem(7, Severity::Error, "cannot find value `totl` in this scope"),
        ];
        let note = new_errors_note("src/lib.rs", &before, &after).unwrap();
        assert!(note.starts_with("Flux: the language server now reports 1 new error in src/lib.rs"));
        assert!(note.contains("- 7:5 error: cannot find value `totl` in this scope"));
        assert!(note.contains("(1 other error in the file was there before the edit.)"));
        assert_eq!(new_errors_note("src/lib.rs", &after, &before), None);
        assert_eq!(new_errors_note("src/lib.rs", &[], &[]), None);
    }

    #[test]
    fn places_show_their_code() {
        let texts = HashMap::from([(
            canonical(Path::new("/p/src/lib.rs")),
            rope("pub fn total() -> u32 {\n    1\n}\n"),
        )]);
        let target = |line: u32, character: u32| NavTarget {
            path: PathBuf::from("/p/src/lib.rs"),
            range: lsp_types::Range::new(
                lsp_types::Position::new(line, character),
                lsp_types::Position::new(line, character + 5),
            ),
        };
        assert_eq!(
            format_places(&[target(1, 4), target(0, 7)], &texts, Some(Path::new("/p"))),
            "src/lib.rs:1:8  pub fn total() -> u32 {\nsrc/lib.rs:2:5  1\n"
        );
    }

    #[test]
    fn hover_becomes_markdown() {
        let contents = HoverContents::Array(vec![
            MarkedString::LanguageString(lsp_types::LanguageString {
                language: "rust".into(),
                value: "pub fn total() -> u32".into(),
            }),
            MarkedString::String("Sums the items.".into()),
        ]);
        assert_eq!(
            hover_text(contents).unwrap(),
            "```rust\npub fn total() -> u32\n```\n\nSums the items."
        );
        assert_eq!(hover_text(HoverContents::Scalar(MarkedString::String(" ".into()))), None);
    }

    #[test]
    fn every_tool_has_a_schema_with_its_required_fields() {
        let specs = specs();
        assert_eq!(specs.len(), 7);
        for spec in &specs {
            assert_eq!(spec.input_schema["type"], "object", "{}", spec.name);
            assert!(spec.read_only, "{}", spec.name);
        }
    }
}
