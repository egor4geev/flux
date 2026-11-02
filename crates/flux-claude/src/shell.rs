//! `!` commands (part 10.2): a command line the user typed after `!` in the message field. The CLI
//! in host mode takes `!` as plain text, so the host runs the command itself — in the project's
//! root, through the user's shell — and hands Claude what the terminal CLI records for its bash
//! mode: a user message `<bash-input>…</bash-input>`, then one with `<bash-stdout>`,
//! `<bash-stderr>` and `<bash-exit-code>` (the shape of the CLI's own headless runner, 2.1.287).
//!
//! The output streams as it comes, decoded and without terminal escapes; Stop ends the whole
//! process group (what the command started in the background too).

use std::io::{self, Read};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use futures::channel::mpsc;

/// How long the end of a command waits for its output to drain: a program it left running in the
/// background may keep the pipes open.
const DRAIN: Duration = Duration::from_secs(2);
/// After SIGTERM, how long a stopped command has before SIGKILL.
#[cfg(unix)]
const KILL_GRACE: Duration = Duration::from_secs(2);
/// What Claude gets of each stream at most (the CLI's own limit for a command's output); a longer
/// one keeps its beginning and its end around a marker.
pub const OUTPUT_LIMIT: usize = 30_000;

/// What a running command reports.
#[derive(Debug, Clone, PartialEq)]
pub enum ShellEvent {
    Stdout(String),
    Stderr(String),
    /// It ended: the exit code (none — a signal ended it) and how long it ran. The last event.
    Exited {
        code: Option<i32>,
        duration: Duration,
    },
}

/// A running command: stops it.
#[derive(Debug, Clone)]
pub struct ShellCommand {
    /// Its process group: the shell's pid.
    group: i32,
    ended: Arc<AtomicBool>,
}

impl ShellCommand {
    /// Ends the command and everything it started: SIGTERM to its group, SIGKILL if it's still
    /// there after a grace period.
    #[cfg(unix)]
    pub fn stop(&self) {
        if self.ended.load(Ordering::SeqCst) || self.group <= 0 {
            return;
        }
        signal(self.group, libc::SIGTERM);
        let (group, ended) = (self.group, self.ended.clone());
        let _ = thread::Builder::new()
            .name("flux shell kill".into())
            .spawn(move || {
                thread::sleep(KILL_GRACE);
                if !ended.load(Ordering::SeqCst) {
                    signal(group, libc::SIGKILL);
                }
            });
    }

    /// Windows has no signals to ask with: the command's whole process tree ends at once.
    #[cfg(windows)]
    pub fn stop(&self) {
        use flux_platform::process::Background;
        if self.ended.load(Ordering::SeqCst) || self.group <= 0 {
            return;
        }
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &self.group.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .background()
            .status();
    }

    pub fn pid(&self) -> u32 {
        self.group as u32
    }
}

#[cfg(unix)]
fn signal(group: i32, signal: libc::c_int) {
    // SAFETY: `kill` only sends a signal; a negative pid is the process group of a command this
    // runner started (a group of its own) and hasn't reaped yet.
    unsafe {
        libc::kill(-group, signal);
    }
}

/// The user's shell: `$SHELL` when it's a path, otherwise zsh (the macOS default).
#[cfg(unix)]
pub fn user_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|shell| shell.starts_with('/'))
        .unwrap_or_else(|| "/bin/zsh".into())
}

/// Windows: the Git Bash the Claude CLI itself runs commands in ([`crate::cli::git_bash`]), so `!`
/// commands read as in Claude's bash mode; without Git — PowerShell.
#[cfg(windows)]
pub fn user_shell() -> String {
    match crate::cli::git_bash() {
        Some(bash) => bash.to_string_lossy().into_owned(),
        None => "powershell.exe".into(),
    }
}

/// How a shell takes a command line: `-c` for the POSIX shells (and Git Bash), `-Command` for
/// PowerShell, `/C` for `cmd`.
fn command_args(shell: &str, command: &str, login: bool) -> Vec<String> {
    let name = Path::new(shell)
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match name.as_str() {
        "pwsh" | "powershell" => [
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            command,
        ]
        .map(String::from)
        .to_vec(),
        "cmd" => ["/D", "/S", "/C", command].map(String::from).to_vec(),
        _ if login => ["-l", "-c", command].map(String::from).to_vec(),
        _ => ["-c", command].map(String::from).to_vec(),
    }
}

/// Runs `command` with `shell -c` in `cwd`. `env` — the whole environment (the login shell's, so
/// that an app started from the Dock has the user's `PATH`); `None` — the shell runs as a login
/// shell with Flux's own environment. No input: a command that reads it gets end of file.
pub fn spawn(
    command: &str,
    cwd: &Path,
    shell: &str,
    env: Option<&[(String, String)]>,
) -> io::Result<(ShellCommand, mpsc::UnboundedReceiver<ShellEvent>)> {
    let mut process = Command::new(shell);
    process
        .args(command_args(shell, command, env.is_none()))
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    process.process_group(0);
    #[cfg(windows)]
    flux_platform::process::Background::background(&mut process);
    if let Some(env) = env {
        process
            .env_clear()
            .envs(env.iter().map(|(name, value)| (name, value)));
    }
    // Plain output: no terminal is there to draw colors or progress on.
    process.env("TERM", "dumb");
    let mut child = process.spawn()?;
    let started = Instant::now();
    let group = child.id() as i32;
    let ended = Arc::new(AtomicBool::new(false));
    let (events, receiver) = mpsc::unbounded();
    let (drained, drains) = std::sync::mpsc::channel();
    if let Some(stdout) = child.stdout.take() {
        read_stream(stdout, events.clone(), ShellEvent::Stdout, drained.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        read_stream(stderr, events.clone(), ShellEvent::Stderr, drained.clone());
    }
    drop(drained);
    let reaped = ended.clone();
    thread::Builder::new()
        .name("flux shell wait".into())
        .spawn(move || {
            let status = child.wait();
            reaped.store(true, Ordering::SeqCst);
            // The output still in the pipes comes first (both readers end), unless something the
            // command left running holds them.
            let deadline = Instant::now() + DRAIN;
            for _ in 0..2 {
                let left = deadline.saturating_duration_since(Instant::now());
                if drains.recv_timeout(left).is_err() {
                    break;
                }
            }
            let code = status.ok().and_then(|status| status.code());
            let _ = events.unbounded_send(ShellEvent::Exited {
                code,
                duration: started.elapsed(),
            });
        })?;
    Ok((ShellCommand { group, ended }, receiver))
}

/// Reads one of the command's streams on a thread of its own, decoded chunk by chunk.
fn read_stream(
    mut stream: impl Read + Send + 'static,
    events: mpsc::UnboundedSender<ShellEvent>,
    event: fn(String) -> ShellEvent,
    drained: std::sync::mpsc::Sender<()>,
) {
    let _ = thread::Builder::new()
        .name("flux shell output".into())
        .spawn(move || {
            let mut decoder = Decoder::default();
            let mut buffer = [0u8; 8192];
            loop {
                match stream.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        let text = decoder.feed(&buffer[..read]);
                        if !text.is_empty() && events.unbounded_send(event(text)).is_err() {
                            break;
                        }
                    }
                }
            }
            let rest = decoder.finish();
            if !rest.is_empty() {
                let _ = events.unbounded_send(event(rest));
            }
            let _ = drained.send(());
        });
}

/// Bytes of a stream → text: UTF-8 across chunk boundaries (invalid bytes become U+FFFD), terminal
/// escapes left out (colors, cursor moves, titles), CR LF as LF. A lone CR stays: whoever shows the
/// text rewrites the line with it ([`append`]).
#[derive(Debug, Default)]
pub struct Decoder {
    /// The start of a character split by the chunk's end.
    partial: Vec<u8>,
    escape: Escape,
    /// The chunk ended with CR: a LF at the start of the next one makes them one line break.
    carriage: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
enum Escape {
    #[default]
    None,
    /// After ESC.
    Start,
    /// A control sequence: `ESC [` … a final byte.
    Csi,
    /// An operating system command: `ESC ]` … BEL or `ESC \`.
    Osc,
    /// ESC inside an operating system command.
    OscEsc,
    /// A two-byte sequence: `ESC (` and the like, one more byte.
    Single,
}

impl Decoder {
    pub fn feed(&mut self, bytes: &[u8]) -> String {
        let mut data = std::mem::take(&mut self.partial);
        data.extend_from_slice(bytes);
        let mut text = String::with_capacity(data.len());
        let mut rest = &data[..];
        loop {
            match std::str::from_utf8(rest) {
                Ok(valid) => {
                    text.push_str(valid);
                    break;
                }
                Err(error) => {
                    let (valid, after) = rest.split_at(error.valid_up_to());
                    // The first `valid_up_to` bytes are UTF-8.
                    text.push_str(std::str::from_utf8(valid).unwrap_or_default());
                    match error.error_len() {
                        Some(len) => {
                            text.push('\u{fffd}');
                            rest = &after[len..];
                        }
                        None => {
                            self.partial = after.to_vec();
                            break;
                        }
                    }
                }
            }
        }
        self.clean(&text)
    }

    /// The end of the stream: what was held back.
    pub fn finish(&mut self) -> String {
        let mut text = String::new();
        if !self.partial.is_empty() {
            text.push('\u{fffd}');
            self.partial.clear();
        }
        if std::mem::take(&mut self.carriage) {
            text.push('\r');
        }
        text
    }

    fn clean(&mut self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        for c in text.chars() {
            match self.escape {
                Escape::None => {}
                Escape::Start => {
                    self.escape = match c {
                        '[' => Escape::Csi,
                        ']' => Escape::Osc,
                        '(' | ')' | '*' | '+' | '#' | '%' => Escape::Single,
                        _ => Escape::None,
                    };
                    continue;
                }
                Escape::Csi => {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        self.escape = Escape::None;
                    }
                    continue;
                }
                Escape::Osc => {
                    match c {
                        '\u{7}' => self.escape = Escape::None,
                        '\u{1b}' => self.escape = Escape::OscEsc,
                        _ => {}
                    }
                    continue;
                }
                Escape::OscEsc => {
                    self.escape = if c == '\\' { Escape::None } else { Escape::Osc };
                    continue;
                }
                Escape::Single => {
                    self.escape = Escape::None;
                    continue;
                }
            }
            if std::mem::take(&mut self.carriage) {
                if c == '\n' {
                    out.push('\n');
                    continue;
                }
                out.push('\r');
            }
            match c {
                '\u{1b}' => self.escape = Escape::Start,
                '\r' => self.carriage = true,
                // Other control characters but tab and line feed draw nothing useful.
                '\n' | '\t' => out.push(c),
                c if c.is_control() => {}
                c => out.push(c),
            }
        }
        out
    }
}

/// Adds decoded output to what is shown: a lone CR goes back to the start of the line, so a
/// progress bar redraws one line instead of filling many.
pub fn append(shown: &mut String, text: &str) {
    let mut parts = text.split('\r');
    if let Some(first) = parts.next() {
        shown.push_str(first);
    }
    for part in parts {
        let line_start = shown.rfind('\n').map_or(0, |at| at + 1);
        shown.truncate(line_start);
        shown.push_str(part);
    }
}

/// The record of the command, as the terminal CLI writes it into the conversation.
pub fn input_record(command: &str) -> String {
    format!("<bash-input>{}</bash-input>", escape(command))
}

/// The record of what the command printed (each stream cut to [`OUTPUT_LIMIT`]) and its exit code
/// (none — it was stopped or ended by a signal).
pub fn output_record(stdout: &str, stderr: &str, code: Option<i32>) -> String {
    let mut record = format!(
        "<bash-stdout>{}</bash-stdout><bash-stderr>{}</bash-stderr>",
        escape(&cut(stdout.trim_end(), OUTPUT_LIMIT)),
        escape(&cut(stderr.trim_end(), OUTPUT_LIMIT)),
    );
    if let Some(code) = code {
        record.push_str(&format!("<bash-exit-code>{code}</bash-exit-code>"));
    }
    record
}

/// The command of a `<bash-input>` record.
pub fn parse_input_record(text: &str) -> Option<String> {
    let text = text.trim();
    if !text.starts_with("<bash-input>") {
        return None;
    }
    Some(unescape(&tag(text, "bash-input")?))
}

/// What a `<bash-stdout>` / `<bash-stderr>` record holds: the output (standard output, then
/// standard error) and the exit code when it's there. A large output the CLI saved to a file
/// (`<persisted-output>`) reads as its note.
pub fn parse_output_record(text: &str) -> Option<(String, Option<i32>)> {
    let text = text.trim();
    if !text.starts_with("<bash-stdout>") && !text.starts_with("<bash-stderr>") {
        return None;
    }
    let stdout = tag(text, "bash-stdout").unwrap_or_default();
    let stdout = tag(&stdout, "persisted-output").unwrap_or(stdout);
    let stderr = tag(text, "bash-stderr").unwrap_or_default();
    let output = [stdout, stderr]
        .into_iter()
        .map(|part| unescape(part.trim_end()))
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let code = tag(text, "bash-exit-code").and_then(|code| code.trim().parse().ok());
    Some((output, code))
}

/// The text inside `<name>…</name>` (to the end when it isn't closed).
fn tag(text: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..]
        .find(&close)
        .map_or(text.len(), |end| start + end);
    Some(text[start..end].to_string())
}

/// `&`, `<` and `>` as entities, as the CLI writes a command's output, so that the output can't
/// close the record's tags.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// `text` within `limit` characters: its beginning and its end around a marker that says how much
/// was left out (the end of a build's log holds the errors).
pub fn cut(text: &str, limit: usize) -> String {
    let count = text.chars().count();
    if count <= limit {
        return text.to_string();
    }
    let head = limit / 3;
    let tail = limit - head;
    let skipped = count - head - tail;
    let start: String = text.chars().take(head).collect();
    let end: String = text.chars().skip(count - tail).collect();
    format!("{start}\n… [{skipped} characters truncated] …\n{end}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use futures::executor::block_on;

    /// A POSIX shell to run the commands in: Git Bash on Windows.
    fn sh() -> String {
        if cfg!(windows) {
            user_shell()
        } else {
            "/bin/sh".into()
        }
    }

    /// How a command ended when Stop ended it: by a signal (no code), or on Windows, which has no
    /// signals, by `taskkill` (code 1).
    const STOPPED: Option<i32> = if cfg!(windows) { Some(1) } else { None };

    fn run(command: &str) -> (String, String, Option<i32>) {
        let cwd = std::env::temp_dir();
        let (_, events) = spawn(command, &cwd, &sh(), None).unwrap();
        let events: Vec<ShellEvent> = block_on(events.collect());
        let (mut stdout, mut stderr, mut code) = (String::new(), String::new(), None);
        for event in events {
            match event {
                ShellEvent::Stdout(text) => stdout.push_str(&text),
                ShellEvent::Stderr(text) => stderr.push_str(&text),
                ShellEvent::Exited { code: exit, .. } => code = exit,
            }
        }
        (stdout, stderr, code)
    }

    #[test]
    fn a_command_prints_and_exits() {
        let (stdout, stderr, code) = run("echo hello; echo oops >&2; exit 3");
        assert_eq!(stdout, "hello\n");
        assert_eq!(stderr, "oops\n");
        assert_eq!(code, Some(3));
    }

    #[test]
    fn a_missing_program_fails_in_the_shell() {
        let (_, stderr, code) = run("definitely-not-a-flux-command-42");
        assert!(stderr.contains("not found"), "{stderr}");
        assert_eq!(code, Some(127));
    }

    #[test]
    fn the_command_runs_in_its_folder_with_the_given_environment() {
        let dir = tempfile::tempdir().unwrap();
        let env = [("FLUX_SHELL_PROBE".to_string(), "42".to_string())];
        // Git Bash prints its own `/c/…` paths unless asked for the Windows one.
        let pwd = if cfg!(windows) { "pwd -W" } else { "pwd" };
        let (_, events) = spawn(
            &format!("{pwd}; echo $FLUX_SHELL_PROBE"),
            dir.path(),
            &sh(),
            Some(&env),
        )
        .unwrap();
        let stdout: String = block_on(events.collect::<Vec<_>>())
            .into_iter()
            .filter_map(|event| match event {
                ShellEvent::Stdout(text) => Some(text),
                _ => None,
            })
            .collect();
        let real = dir.path().canonicalize().unwrap();
        if cfg!(windows) {
            // `C:/Users/RUNNER~1/…`: the same folder, spelled another way.
            let (folder, rest) = stdout.split_once('\n').unwrap();
            assert_eq!(Path::new(folder).canonicalize().unwrap(), real);
            assert_eq!(rest, "42\n");
        } else {
            assert_eq!(stdout, format!("{}\n42\n", real.display()));
        }
    }

    #[test]
    fn stop_ends_the_command_and_what_it_started() {
        let cwd = std::env::temp_dir();
        let started = Instant::now();
        let (command, mut events) =
            spawn("sleep 30 & sleep 30; echo never", &cwd, &sh(), None).unwrap();
        thread::sleep(Duration::from_millis(200));
        command.stop();
        let exited = block_on(async {
            while let Some(event) = events.next().await {
                if let ShellEvent::Exited { code, .. } = event {
                    return Some(code);
                }
            }
            None
        });
        assert_eq!(exited, Some(STOPPED), "ended by Stop");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn the_decoder_drops_escapes_and_joins_split_characters() {
        let mut decoder = Decoder::default();
        let bytes = "\u{1b}[1;31mошибка\u{1b}[0m\r\n\u{1b}]0;title\u{7}ok\n".as_bytes();
        // Split inside a Cyrillic letter, inside an escape and between CR and LF.
        let mut text = String::new();
        for chunk in [&bytes[..9], &bytes[9..13], &bytes[13..24], &bytes[24..]] {
            text.push_str(&decoder.feed(chunk));
        }
        text.push_str(&decoder.finish());
        assert_eq!(text, "ошибка\nok\n");
    }

    #[test]
    fn a_carriage_return_rewrites_the_line() {
        let mut shown = String::from("build\n");
        let mut decoder = Decoder::default();
        for chunk in ["10%", "\r20%\r", "100%\ndone\n"] {
            append(&mut shown, &decoder.feed(chunk.as_bytes()));
        }
        assert_eq!(shown, "build\n100%\ndone\n");
    }

    #[test]
    fn records_read_back_as_written() {
        let input = input_record("grep '<a>' x && echo ok");
        assert_eq!(
            input,
            "<bash-input>grep '&lt;a&gt;' x &amp;&amp; echo ok</bash-input>"
        );
        assert_eq!(
            parse_input_record(&input).as_deref(),
            Some("grep '<a>' x && echo ok")
        );
        let output = output_record("a < b\n", "warning\n", Some(1));
        assert_eq!(
            output,
            "<bash-stdout>a &lt; b</bash-stdout><bash-stderr>warning</bash-stderr>\
             <bash-exit-code>1</bash-exit-code>"
        );
        assert_eq!(
            parse_output_record(&output),
            Some(("a < b\nwarning".into(), Some(1)))
        );
        // The terminal CLI's own record has no exit code.
        assert_eq!(
            parse_output_record("<bash-stdout>hi</bash-stdout><bash-stderr></bash-stderr>"),
            Some(("hi".into(), None))
        );
    }

    #[test]
    fn long_output_keeps_its_beginning_and_end() {
        let text: String = (0..1000).map(|line| format!("line {line}\n")).collect();
        let cut = cut(&text, 300);
        assert!(cut.starts_with("line 0\n"));
        assert!(cut.ends_with("line 999\n"));
        assert!(cut.contains("characters truncated"));
        assert!(cut.chars().count() < 400);
    }
}
