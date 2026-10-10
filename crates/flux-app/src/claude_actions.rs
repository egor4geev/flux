//! Claude around the editor (part 9.2): Explain, Fix and the other ready requests — from the
//! editor's context menu (Claude ▸), ⌥↵, the problem popup (hover, F2), the palette — each start a
//! new session with the request sent at once (the author's choice, as AI Actions of JetBrains AI
//! Assistant); "Send to Claude" of the tree and the tabs and files dropped on a chat go into the
//! current chat's message as mentions.
//!
//! A request is written in the language of the interface (the author's is Russian) and names the
//! code as the CLI reads it: `@src/main.rs#L12-20` — the selected lines; without a selection, the
//! whole file (Explain, Find Problems, Write Tests), the caret's line (Add Documentation) or the
//! problems on it (Fix). The session is named after the request ("Explain main.rs:12–20").

use std::path::{Path, PathBuf};

use flux_claude::UserInput;
use gpui::{Action, App, Context, Div, Entity, InteractiveElement, Window, actions};

use crate::claude;
use crate::diagnostics::{Diagnostic, Severity};
use crate::editor::Editor;
use crate::i18n::{tr, trf};
use crate::workspace::Workspace;

actions!(
    claude,
    [
        /// The selection (or the file) explained, in a new session.
        ExplainWithClaude,
        /// The problems in the selection (or on the caret's line) fixed, in a new session.
        FixWithClaude,
        /// The selection (or the file) reviewed for bugs, in a new session.
        FindProblemsWithClaude,
        /// Tests written for the selection (or the file), in a new session.
        WriteTestsWithClaude,
        /// Documentation written for the selection (or the declaration on the caret's line), in a
        /// new session.
        DocumentWithClaude,
    ]
);

/// Files and folders go into the current chat's message as mentions (`@src/main.rs`,
/// `@src/ui/`): "Send to Claude" of the tree and the tabs.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = claude, no_json)]
pub struct SendPathsToClaude(pub Vec<PathBuf>);

/// Problems picked in the editor (the hover, F2) fixed by Claude, in a new session.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = claude, no_json)]
pub struct FixProblemsWithClaude {
    /// The file (absolute).
    pub path: PathBuf,
    pub problems: Vec<Problem>,
}

/// A problem as a request names it: where (1-based), how bad, what.
#[derive(Clone, PartialEq, Debug)]
pub struct Problem {
    pub line: usize,
    pub column: usize,
    pub severity: Severity,
    pub message: String,
    /// "rustc E0308".
    pub origin: Option<String>,
}

impl Problem {
    /// A diagnostic of `editor`'s document.
    pub fn of(diagnostic: &Diagnostic, editor: &Editor) -> Problem {
        let text = editor.document.text();
        let start = diagnostic.range.start.min(text.len_chars());
        let line = text.char_to_line(start);
        let origin = [diagnostic.source.as_deref(), diagnostic.code.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        Problem {
            line: line + 1,
            column: start - text.line_to_char(line) + 1,
            severity: diagnostic.severity,
            message: diagnostic.message.trim().to_string(),
            origin: (!origin.is_empty()).then_some(origin),
        }
    }
}

/// The ready requests of the menus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    Explain,
    Fix,
    FindProblems,
    WriteTests,
    Document,
}

/// The code a request is about: the file as the CLI reads it (relative to the project root,
/// absolute outside it) and its lines (1-based, inclusive); `None` — the whole file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Code {
    pub path: String,
    pub lines: Option<(usize, usize)>,
}

impl Code {
    /// "main.rs:12–20": the file's name and lines, for the session's title.
    fn short(&self) -> String {
        let name = Path::new(&self.path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.clone());
        match self.lines {
            None => name,
            Some((first, last)) if first == last => format!("{name}:{first}"),
            Some((first, last)) => format!("{name}:{first}–{last}"),
        }
    }

    fn mention(&self) -> String {
        claude::mention(&self.path, self.lines)
    }
}

/// A request for Claude and the title of its session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub title: String,
    pub text: String,
}

/// The request of `ask` about `code`; Fix lists `problems` (none — no request).
pub fn request(ask: Ask, code: &Code, problems: &[Problem]) -> Option<Request> {
    let mention = code.mention();
    let short = code.short();
    let request = match ask {
        Ask::Explain => Request {
            title: trf("Explain {0}", &[&short]),
            text: trf("Explain what the code in {0} does and how it works.", &[&mention]),
        },
        Ask::Fix => {
            let first = problems.first()?;
            let first_line = first.message.lines().next().unwrap_or_default();
            Request {
                title: trf("Fix: {0}", &[&shorten(first_line, 48)]),
                text: trf(
                    "Fix the problems in {0} the language server reports:\n{1}",
                    &[&mention, &problem_list(problems)],
                ),
            }
        }
        Ask::FindProblems => Request {
            title: trf("Find problems in {0}", &[&short]),
            text: trf(
                "Review the code in {0} for bugs and problems. List what you find with the lines; don't change the code yet.",
                &[&mention],
            ),
        },
        Ask::WriteTests => Request {
            title: trf("Tests for {0}", &[&short]),
            text: trf(
                "Write tests for the code in {0} in the style of the project's existing tests and run them.",
                &[&mention],
            ),
        },
        Ask::Document => Request {
            title: trf("Document {0}", &[&short]),
            text: trf(
                "Write documentation comments for the code in {0} in the style of the project.",
                &[&mention],
            ),
        },
    };
    Some(request)
}

/// "- 12:5 Error: mismatched types (rustc E0308)", one problem a line; a message of several lines
/// keeps them, indented.
fn problem_list(problems: &[Problem]) -> String {
    problems
        .iter()
        .map(|problem| {
            let message = problem.message.replace('\n', "\n  ");
            let origin = problem
                .origin
                .as_ref()
                .map(|origin| format!(" ({origin})"))
                .unwrap_or_default();
            format!(
                "- {}:{} {}: {message}{origin}",
                problem.line,
                problem.column,
                problem.severity.label()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max - 1).collect();
    format!("{}…", cut.trim_end())
}

/// The file of `editor` as the CLI reads it: relative to `root`, absolute outside it.
pub fn cli_path(path: &Path, root: Option<&Path>) -> String {
    root.and_then(|root| path.strip_prefix(root).ok())
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// The selected lines of `editor` (1-based, inclusive); a selection ending at the start of a line
/// doesn't take that line. `None` — nothing selected.
pub fn selected_lines(editor: &Editor) -> Option<(usize, usize)> {
    let selection = editor.document.selection().primary();
    let (start, end) = (selection.from(), selection.to());
    if start == end {
        return None;
    }
    let text = editor.document.text();
    let first = text.char_to_line(start) + 1;
    let last = text.char_to_line(end.saturating_sub(1).max(start)) + 1;
    Some((first, last))
}

/// The caret's line (1-based).
fn caret_line(editor: &Editor) -> usize {
    let text = editor.document.text();
    let head = editor.document.selection().primary().head.min(text.len_chars());
    text.char_to_line(head) + 1
}

/// The problems Fix takes: in the selected lines, else on the caret's line; hints aren't.
pub fn problems_here(editor: &Editor) -> Vec<Problem> {
    let text = editor.document.text();
    let (first, last) = selected_lines(editor).unwrap_or_else(|| {
        let line = caret_line(editor);
        (line, line)
    });
    let start = text.line_to_char(first - 1);
    let end = if last < text.len_lines() {
        text.line_to_char(last)
    } else {
        text.len_chars()
    };
    editor
        .diagnostics
        .in_range(start..end)
        .into_iter()
        .filter(|diagnostic| diagnostic.severity != Severity::Hint)
        .map(|diagnostic| Problem::of(diagnostic, editor))
        .collect()
}

/// The lines a request about `editor`'s code names: the selection's, else what fits the request.
fn lines_for(ask: Ask, editor: &Editor, problems: &[Problem]) -> Option<(usize, usize)> {
    if let Some(lines) = selected_lines(editor) {
        return Some(lines);
    }
    match ask {
        Ask::Explain | Ask::FindProblems | Ask::WriteTests => None,
        Ask::Document => Some((caret_line(editor), caret_line(editor))),
        Ask::Fix => {
            let first = problems.iter().map(|problem| problem.line).min()?;
            let last = problems.iter().map(|problem| problem.line).max()?;
            Some((first, last))
        }
    }
}

pub fn init(_cx: &mut App) {}

/// The handlers of the window (registered while Claude Code is on).
pub fn actions(root: Div, cx: &mut Context<Workspace>) -> Div {
    root.on_action(cx.listener(|this, _: &ExplainWithClaude, window, cx| {
        ask_about_editor(this, Ask::Explain, window, cx)
    }))
    .on_action(cx.listener(|this, _: &FixWithClaude, window, cx| {
        ask_about_editor(this, Ask::Fix, window, cx)
    }))
    .on_action(cx.listener(|this, _: &FindProblemsWithClaude, window, cx| {
        ask_about_editor(this, Ask::FindProblems, window, cx)
    }))
    .on_action(cx.listener(|this, _: &WriteTestsWithClaude, window, cx| {
        ask_about_editor(this, Ask::WriteTests, window, cx)
    }))
    .on_action(cx.listener(|this, _: &DocumentWithClaude, window, cx| {
        ask_about_editor(this, Ask::Document, window, cx)
    }))
    .on_action(cx.listener(|this, action: &FixProblemsWithClaude, window, cx| {
        let root = this.root().map(Path::to_path_buf);
        let first = action.problems.iter().map(|problem| problem.line).min();
        let last = action.problems.iter().map(|problem| problem.line).max();
        let code = Code {
            path: cli_path(&action.path, root.as_deref()),
            lines: first.zip(last),
        };
        if let Some(request) = request(Ask::Fix, &code, &action.problems) {
            send(this, request, window, cx);
        }
    }))
    .on_action(cx.listener(|this, action: &SendPathsToClaude, window, cx| {
        let root = this.root().map(Path::to_path_buf);
        let mentions = action
            .0
            .iter()
            .map(|path| path_mention(path, root.as_deref()))
            .collect();
        this.send_mentions_to_claude(mentions, window, cx);
    }))
}

/// `@src/main.rs`, `@src/ui/` (a folder), `@"My Notes/a.md"` (with spaces).
pub fn path_mention(path: &Path, root: Option<&Path>) -> String {
    let mut shown = cli_path(path, root);
    if path.is_dir() && !shown.ends_with('/') {
        shown.push('/');
    }
    if shown.chars().any(char::is_whitespace) {
        format!("@\"{shown}\"")
    } else {
        format!("@{shown}")
    }
}

/// A request about the active document's selection, in a new session.
fn ask_about_editor(
    workspace: &mut Workspace,
    ask: Ask,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(editor) = workspace.active_editor() else {
        return;
    };
    let root = workspace.root().map(Path::to_path_buf);
    let request = {
        let editor = editor.read(cx);
        let Some(path) = editor.document.path() else {
            workspace.show_message(tr("Save the file first: Claude reads it from the disk").into(), cx);
            return;
        };
        let problems = if ask == Ask::Fix {
            problems_here(editor)
        } else {
            Vec::new()
        };
        if ask == Ask::Fix && problems.is_empty() {
            workspace.show_message(tr("No problems here").into(), cx);
            return;
        }
        let code = Code {
            path: cli_path(path, root.as_deref()),
            lines: lines_for(ask, editor, &problems),
        };
        request(ask, &code, &problems)
    };
    if let Some(request) = request {
        send(workspace, request, window, cx);
    }
}

/// The request goes to a new session, named after it.
fn send(workspace: &mut Workspace, request: Request, window: &mut Window, cx: &mut Context<Workspace>) {
    let input = UserInput {
        text: request.text,
        images: Vec::new(),
        priority: None,
    };
    if let Some(session) = workspace.ask_claude(input, window, cx) {
        name_session(&session, request.title, cx);
    }
}

fn name_session(
    session: &Entity<crate::claude_session::ClaudeSession>,
    title: String,
    cx: &mut App,
) {
    session.update(cx, |session, cx| session.rename(title, cx));
}

/// Whether the Claude items of the menus are offered.
pub fn offered(cx: &App) -> bool {
    claude::enabled(cx)
}

/// The action of a ready request.
pub fn action_of(ask: Ask) -> Box<dyn Action> {
    match ask {
        Ask::Explain => Box::new(ExplainWithClaude),
        Ask::Fix => Box::new(FixWithClaude),
        Ask::FindProblems => Box::new(FindProblemsWithClaude),
        Ask::WriteTests => Box::new(WriteTestsWithClaude),
        Ask::Document => Box::new(DocumentWithClaude),
    }
}

/// The menu label of a ready request.
pub fn label_of(ask: Ask) -> &'static str {
    match ask {
        Ask::Explain => tr("Explain with Claude"),
        Ask::Fix => tr("Fix with Claude"),
        Ask::FindProblems => tr("Find Problems with Claude"),
        Ask::WriteTests => tr("Write Tests with Claude"),
        Ask::Document => tr("Add Documentation with Claude"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn problem(line: usize, message: &str) -> Problem {
        Problem {
            line,
            column: 5,
            severity: Severity::Error,
            message: message.into(),
            origin: Some("rustc E0308".into()),
        }
    }

    #[test]
    fn requests_name_the_code_as_the_cli_reads_it() {
        let lines = Code {
            path: "src/main.rs".into(),
            lines: Some((12, 20)),
        };
        let explain = request(Ask::Explain, &lines, &[]).unwrap();
        assert_eq!(explain.title, "Explain main.rs:12–20");
        assert!(explain.text.contains("@src/main.rs#L12-20"), "{}", explain.text);

        let file = Code {
            path: "src/main.rs".into(),
            lines: None,
        };
        let tests = request(Ask::WriteTests, &file, &[]).unwrap();
        assert_eq!(tests.title, "Tests for main.rs");
        assert!(tests.text.contains("@src/main.rs "), "{}", tests.text);
    }

    #[test]
    fn fix_lists_the_problems_and_needs_one() {
        let code = Code {
            path: "src/lib.rs".into(),
            lines: Some((7, 7)),
        };
        assert_eq!(request(Ask::Fix, &code, &[]), None);
        let fix = request(
            Ask::Fix,
            &code,
            &[problem(7, "mismatched types\nexpected `u32`, found `&str`")],
        )
        .unwrap();
        assert_eq!(fix.title, "Fix: mismatched types");
        assert!(
            fix.text.ends_with(
                "- 7:5 Error: mismatched types\n  expected `u32`, found `&str` (rustc E0308)"
            ),
            "{}",
            fix.text
        );
        assert!(fix.text.contains("@src/lib.rs#L7 "), "{}", fix.text);
    }

    #[test]
    fn a_mention_is_never_glued_to_punctuation() {
        // The CLI reads `@path` up to a space: `@src/a.rs,` would name another file.
        let code = Code {
            path: "src/a.rs".into(),
            lines: Some((3, 4)),
        };
        for ask in [
            Ask::Explain,
            Ask::Fix,
            Ask::FindProblems,
            Ask::WriteTests,
            Ask::Document,
        ] {
            let text = request(ask, &code, &[problem(3, "x")]).unwrap().text;
            let after = text.split("@src/a.rs#L3-4").nth(1).unwrap();
            assert!(after.starts_with(char::is_whitespace), "{ask:?}: {text}");
        }
    }

    #[test]
    fn paths_are_relative_inside_the_project() {
        let root = Path::new("/p");
        assert_eq!(cli_path(Path::new("/p/src/a.rs"), Some(root)), "src/a.rs");
        assert_eq!(cli_path(Path::new("/other/b.rs"), Some(root)), "/other/b.rs");
        assert_eq!(
            path_mention(Path::new("/p/My Notes.md"), Some(root)),
            "@\"My Notes.md\""
        );
        assert_eq!(shorten("abcdef", 4), "abc…");
    }
}
