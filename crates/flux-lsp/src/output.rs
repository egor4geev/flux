//! What a server said, for "Show Log" (part 10.5): its stderr, its `window/logMessage` and
//! `window/showMessage`, and what Flux noted (started, the server's status, exited) — the last few
//! thousand lines, each with the time since the start. The window keeps it after the server is
//! gone: the log of a server that crashed or couldn't load the project is the one worth reading.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use lsp_types::MessageType;

/// How many lines a log keeps; older ones go.
const MAX_LINES: usize = 5000;
/// A longer line is cut: a server may print a whole JSON document on one line.
const MAX_LINE_CHARS: usize = 2000;

/// Where a line of the log comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogSource {
    /// The server's stderr.
    Stderr,
    /// `window/logMessage` or `window/showMessage`, by its type.
    Message(MessageType),
    /// Flux itself: started, the server's status, exited.
    Flux,
}

impl LogSource {
    fn label(self) -> &'static str {
        match self {
            LogSource::Stderr => "stderr",
            LogSource::Message(MessageType::ERROR) => "error",
            LogSource::Message(MessageType::WARNING) => "warning",
            LogSource::Message(MessageType::INFO) => "info",
            LogSource::Message(_) => "log",
            LogSource::Flux => "flux",
        }
    }
}

struct Line {
    at: Duration,
    source: LogSource,
    text: String,
}

/// A server's log; shared between the client's threads and the window.
pub struct ServerLog {
    started: Instant,
    lines: Mutex<VecDeque<Line>>,
}

impl Default for ServerLog {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerLog {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            lines: Mutex::new(VecDeque::new()),
        }
    }

    fn lines(&self) -> MutexGuard<'_, VecDeque<Line>> {
        self.lines.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Adds `text`; several lines become several entries.
    pub fn push(&self, source: LogSource, text: &str) {
        let at = self.started.elapsed();
        let mut lines = self.lines();
        for line in text.lines() {
            let line = line.trim_end();
            let text = match line.char_indices().nth(MAX_LINE_CHARS) {
                Some((end, _)) => format!("{}…", &line[..end]),
                None => line.to_string(),
            };
            if lines.len() == MAX_LINES {
                lines.pop_front();
            }
            lines.push_back(Line { at, source, text });
        }
    }

    pub fn is_empty(&self) -> bool {
        self.lines().is_empty()
    }

    /// The log as text, oldest first: `[  12.345] stderr  …`.
    pub fn text(&self) -> String {
        let lines = self.lines();
        let mut text = String::with_capacity(lines.iter().map(|line| line.text.len() + 20).sum());
        for line in lines.iter() {
            let _ = writeln!(
                text,
                "[{:8.3}] {:<7} {}",
                line.at.as_secs_f64(),
                line.source.label(),
                line.text
            );
        }
        text
    }

    /// The last error the server printed on stderr in the form of cargo and rustc ("error: …",
    /// then " --> file:line:column"): "unclosed table, expected `]` (Cargo.toml:5:14)". What
    /// rust-analyzer couldn't load a project over is only there — its status just says "Failed
    /// to load workspaces.".
    pub fn last_error(&self) -> Option<String> {
        let lines = self.lines();
        let stderr: Vec<&str> = lines
            .iter()
            .filter(|line| line.source == LogSource::Stderr)
            .map(|line| line.text.as_str())
            .collect();
        last_error_in(&stderr)
    }
}

/// [`ServerLog::last_error`] over the stderr lines, oldest first: the first error of the last block
/// of cargo's output (the cause comes first, "failed searching for potential workspace" after it),
/// with its place and the block's hint, if any. A block starts after a line of the server's own
/// log (a timestamp) or its "stderr:" heading; rust-analyzer's last log line may carry the cause
/// itself ("… `cargo metadata` exited with an error: error: unclosed array…").
fn last_error_in(lines: &[&str]) -> Option<String> {
    const INLINE: &str = "error: ";
    let is_error = |line: &str| {
        let line = line.trim_start();
        line.starts_with("error:") || line.starts_with("error[")
    };
    let starts_block = |line: &str| {
        let line = line.trim_start();
        line == "stderr:"
            || line
                .get(..4)
                .is_some_and(|year| year.parse::<u16>().is_ok())
    };
    let last = lines.iter().rposition(|line| is_error(line))?;
    // The block of the last error: its first line, and the log line that heads it.
    let mut start = 0;
    let mut first = last;
    for index in (0..last).rev() {
        if starts_block(lines[index]) {
            start = index + 1;
            break;
        }
        if is_error(lines[index]) {
            first = index;
        }
    }
    let end = lines[last..]
        .iter()
        .position(|line| starts_block(line))
        .map_or(lines.len(), |offset| last + offset);
    let inline = start
        .checked_sub(1)
        .map(|heading| lines[heading])
        .and_then(|heading| {
            heading
                .rfind(INLINE)
                .map(|at| &heading[at + INLINE.len()..])
        });
    let (message, after) = match inline {
        Some(message) => (message.trim(), start),
        None => {
            let line = lines[first].trim();
            let message = line.split_once(": ").map_or(line, |(_, message)| message);
            (message.trim(), first + 1)
        }
    };
    if message.is_empty() {
        return None;
    }
    let location = lines
        .get(after)
        .and_then(|next| next.trim_start().strip_prefix("-->"))
        .map(str::trim)
        .filter(|location| !location.is_empty());
    let mut text = match location {
        Some(location) => format!("{message} ({location})"),
        None => message.to_string(),
    };
    let help = lines[start..end]
        .iter()
        .find_map(|line| line.trim_start().strip_prefix("help:"))
        .map(str::trim)
        .filter(|help| !help.is_empty());
    if let Some(help) = help {
        text.push('\n');
        let mut chars = help.chars();
        if let Some(first) = chars.next() {
            text.extend(first.to_uppercase());
            text.push_str(chars.as_str());
        }
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_kept_with_their_source_and_the_oldest_go() {
        let log = ServerLog::new();
        assert!(log.is_empty());
        log.push(LogSource::Flux, "started");
        log.push(LogSource::Stderr, "one\ntwo  ");
        log.push(LogSource::Message(MessageType::WARNING), "careful");
        let text = log.text();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4);
        assert!(lines[0].ends_with("] flux    started"), "{text}");
        assert!(lines[2].ends_with("] stderr  two"), "{text}");
        assert!(lines[3].ends_with("] warning careful"), "{text}");

        for i in 0..MAX_LINES + 10 {
            log.push(LogSource::Stderr, &i.to_string());
        }
        let text = log.text();
        assert_eq!(text.lines().count(), MAX_LINES);
        assert!(
            text.lines()
                .last()
                .unwrap()
                .ends_with(&(MAX_LINES + 9).to_string())
        );
        assert!(!text.contains("started"));
    }

    #[test]
    fn long_lines_are_cut() {
        let log = ServerLog::new();
        log.push(LogSource::Stderr, &"я".repeat(MAX_LINE_CHARS + 5));
        let text = log.text();
        assert!(text.trim_end().ends_with('…'));
        assert_eq!(text.chars().filter(|c| *c == 'я').count(), MAX_LINE_CHARS);
    }

    #[test]
    fn the_first_cargo_error_of_the_last_block_with_its_place_and_hint() {
        // What rust-analyzer prints when a Cargo.toml above the project is broken (twice).
        let block = [
            "stderr:",
            "error: unclosed array, expected `]`",
            " --> ../Cargo.toml:7:46",
            "  |",
            "7 | serde = { version = \"1\", features = [\"derive\"",
            "error: failed searching for potential workspace",
            "help: to avoid searching for a non-existent workspace, add `[workspace]`",
            "",
        ];
        let mut stderr =
            vec!["2026-10-15T10:26:53.43294+03:00 ERROR failed fetching cargo workspace"];
        stderr.extend(block);
        stderr.push("2026-10-15T10:26:53.487895+03:00  WARN failed to run `cargo metadata`");
        stderr.extend(block);
        assert_eq!(
            last_error_in(&stderr).as_deref(),
            Some(
                "unclosed array, expected `]` (../Cargo.toml:7:46)\nTo avoid searching for a \
                 non-existent workspace, add `[workspace]`"
            )
        );
        // rust-analyzer's own error last: the cause is at the end of its line.
        let mut ra = stderr.clone();
        ra.extend([
            "2026-10-15T10:43:57.711534+03:00 ERROR FetchWorkspaceError: rust-analyzer failed to \
             load workspace: Failed to run `cargo metadata`: `cargo metadata` exited with an \
             error: error: unclosed array, expected `]`",
            " --> ../Cargo.toml:7:46",
            "  |",
            "error: failed searching for potential workspace",
            "help: to avoid searching for a non-existent workspace, add `[workspace]`",
        ]);
        assert_eq!(
            last_error_in(&ra).as_deref(),
            Some(
                "unclosed array, expected `]` (../Cargo.toml:7:46)\nTo avoid searching for a \
                 non-existent workspace, add `[workspace]`"
            )
        );
        // A broken Cargo.toml of the project itself: no hint.
        let own = [
            "2026-10-15T10:28:00.970968+03:00 ERROR failed fetching cargo workspace root",
            "stderr:",
            "error: unclosed table, expected `]`",
            " --> Cargo.toml:5:14",
            "  |",
            "5 | [dependencies",
        ];
        assert_eq!(
            last_error_in(&own).as_deref(),
            Some("unclosed table, expected `]` (Cargo.toml:5:14)")
        );
        assert_eq!(last_error_in(&["all good", "ERROR in a log line"]), None);
        assert_eq!(
            last_error_in(&["error[E0433]: failed to resolve"]).as_deref(),
            Some("failed to resolve")
        );
    }

    #[test]
    fn only_stderr_counts_for_the_last_error() {
        let log = ServerLog::new();
        log.push(LogSource::Stderr, "error: broken\n --> Cargo.toml:1:1");
        log.push(
            LogSource::Message(MessageType::ERROR),
            "error: from a message",
        );
        assert_eq!(log.last_error().as_deref(), Some("broken (Cargo.toml:1:1)"));
    }
}
