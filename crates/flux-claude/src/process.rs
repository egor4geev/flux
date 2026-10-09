//! One `claude` process in host mode: JSON lines over stdin and stdout on background threads (no
//! async runtime, as flux-lsp — ADR-019). Frames come out of a `futures` channel the window's UI
//! task awaits; Flux's control requests get their answers through one-shot channels.
//!
//! `FLUX_CLAUDE_LOG=1` prints every line in both directions and the CLI's stderr to Flux's stderr.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use futures::channel::{mpsc as async_mpsc, oneshot};
use serde_json::Value;

use crate::cli::{Cli, LaunchOptions};
use crate::protocol::{self, HostRequest, Incoming};
use crate::types::UserInput;

/// How many last lines of the CLI's stderr are kept for the exit reason.
const STDERR_LINES: usize = 20;
/// After stdin closes, the CLI gets this long to finish before it is killed.
const EXIT_GRACE: Duration = Duration::from_secs(5);

/// What the process sends the window.
#[derive(Debug, Clone, PartialEq)]
pub enum ProcessEvent {
    Frame(Box<Incoming>),
    /// The process is gone. `stderr` — its last lines (why it failed to start, a crash).
    Exited {
        code: Option<i32>,
        stderr: Vec<String>,
    },
}

type Waiters = Arc<Mutex<HashMap<String, oneshot::Sender<Result<Value, String>>>>>;

/// A running `claude`. Dropping it closes stdin and kills the process if it doesn't exit.
pub struct Process {
    child: Arc<Mutex<Child>>,
    stdin: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
    waiters: Waiters,
    next_request: AtomicU64,
    pid: u32,
    log: bool,
}

impl Process {
    /// Starts `claude` in host mode in `options.cwd`. The first request should be
    /// [`HostRequest::Initialize`]; user messages may follow at once.
    pub fn spawn(
        cli: &Cli,
        options: &LaunchOptions,
    ) -> io::Result<(Process, async_mpsc::UnboundedReceiver<ProcessEvent>)> {
        let log = std::env::var_os("FLUX_CLAUDE_LOG").is_some_and(|value| value == "1");
        let mut command = cli.command();
        command
            .args(options.args())
            .current_dir(&options.cwd)
            // `session_state_changed` (idle / running / requires_action), as the SDK asks for it.
            .env("CLAUDE_CODE_SDK_READS_SESSION_STATE", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if log {
            eprintln!(
                "[claude] spawn {} {}",
                cli.path.display(),
                options.args().join(" ")
            );
        }
        let mut child = command.spawn()?;
        let pid = child.id();
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        let child = Arc::new(Mutex::new(child));
        let waiters: Waiters = Arc::default();
        let (events, receiver) = async_mpsc::unbounded();

        let (writer, lines) = mpsc::channel::<Vec<u8>>();
        spawn_writer(stdin, lines, log)?;
        let stderr_lines = spawn_stderr(stderr, log)?;
        spawn_reader(
            stdout,
            child.clone(),
            waiters.clone(),
            stderr_lines,
            events,
            log,
        )?;

        let process = Process {
            child,
            stdin: Mutex::new(Some(writer)),
            waiters,
            next_request: AtomicU64::new(1),
            pid,
            log,
        };
        Ok((process, receiver))
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Sends the user's message and returns its uuid (the CLI's lifecycle frames refer to it).
    pub fn send_user(&self, input: &UserInput) -> String {
        let uuid = new_uuid();
        self.write(&protocol::user_message(&uuid, input));
        uuid
    }

    /// A control request; the answer comes through the returned channel (canceled — the process
    /// is gone).
    pub fn request(&self, request: &HostRequest) -> oneshot::Receiver<Result<Value, String>> {
        let id = format!("flux-{}", self.next_request.fetch_add(1, Ordering::Relaxed));
        let (sender, receiver) = oneshot::channel();
        lock(&self.waiters).insert(id.clone(), sender);
        self.write(&protocol::control_request(&id, request));
        receiver
    }

    /// Answers one of the CLI's requests.
    pub fn respond(&self, id: &str, response: Value) {
        self.write(&protocol::control_success(id, response));
    }

    /// Refuses one of the CLI's requests.
    pub fn respond_error(&self, id: &str, error: &str) {
        self.write(&protocol::control_error(id, error));
    }

    /// Closes stdin: the CLI finishes and exits (killed if it doesn't within a few seconds).
    pub fn close(&self) {
        if lock(&self.stdin).take().is_none() {
            return;
        }
        let child = self.child.clone();
        thread::Builder::new()
            .name("claude-exit".into())
            .spawn(move || {
                let deadline = Instant::now() + EXIT_GRACE;
                while Instant::now() < deadline {
                    if !matches!(lock(&child).try_wait(), Ok(None)) {
                        return;
                    }
                    thread::sleep(Duration::from_millis(50));
                }
                let _ = lock(&child).kill();
            })
            .ok();
    }

    /// Kills the process at once.
    pub fn kill(&self) {
        lock(&self.stdin).take();
        let _ = lock(&self.child).kill();
    }

    fn write(&self, frame: &Value) {
        let mut line = frame.to_string();
        if self.log {
            eprintln!("[claude] > {line}");
        }
        line.push('\n');
        if let Some(stdin) = lock(&self.stdin).as_ref() {
            stdin.send(line.into_bytes()).ok();
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.close();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

fn spawn_writer(stdin: ChildStdin, lines: mpsc::Receiver<Vec<u8>>, _log: bool) -> io::Result<()> {
    thread::Builder::new()
        .name("claude-writer".into())
        .spawn(move || {
            let mut stdin = stdin;
            for line in lines {
                if stdin.write_all(&line).and_then(|()| stdin.flush()).is_err() {
                    break;
                }
            }
            // The channel closed: dropping stdin tells the CLI to finish.
        })?;
    Ok(())
}

fn spawn_stderr(
    stderr: impl Read + Send + 'static,
    log: bool,
) -> io::Result<Arc<Mutex<VecDeque<String>>>> {
    let lines = Arc::new(Mutex::new(VecDeque::new()));
    let shared = lines.clone();
    thread::Builder::new()
        .name("claude-stderr".into())
        .spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                if log {
                    eprintln!("[claude] stderr: {line}");
                }
                if line.trim().is_empty() {
                    continue;
                }
                let mut lines = lock(&shared);
                if lines.len() == STDERR_LINES {
                    lines.pop_front();
                }
                lines.push_back(line);
            }
        })?;
    Ok(lines)
}

fn spawn_reader(
    stdout: impl Read + Send + 'static,
    child: Arc<Mutex<Child>>,
    waiters: Waiters,
    stderr: Arc<Mutex<VecDeque<String>>>,
    events: async_mpsc::UnboundedSender<ProcessEvent>,
    log: bool,
) -> io::Result<()> {
    thread::Builder::new()
        .name("claude-reader".into())
        .spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                if log {
                    eprintln!("[claude] < {}", line.trim_end());
                }
                let Some(frame) = protocol::parse_line(&line) else {
                    continue;
                };
                // Answers to Flux's own requests go to whoever waits for them.
                if let Incoming::Response { id, result } = &frame
                    && let Some(waiter) = lock(&waiters).remove(id)
                {
                    waiter.send(result.clone()).ok();
                    continue;
                }
                if events
                    .unbounded_send(ProcessEvent::Frame(Box::new(frame)))
                    .is_err()
                {
                    break;
                }
            }
            // stdout closed: the process is exiting.
            let deadline = Instant::now() + EXIT_GRACE;
            let code = loop {
                match lock(&child).try_wait() {
                    Ok(Some(status)) => break status.code(),
                    Ok(None) if Instant::now() < deadline => {
                        thread::sleep(Duration::from_millis(20))
                    }
                    _ => {
                        let mut child = lock(&child);
                        let _ = child.kill();
                        break child.wait().ok().and_then(|status| status.code());
                    }
                }
            };
            // Requests left without an answer are canceled (their senders drop).
            lock(&waiters).clear();
            // Give the stderr thread a moment to read the last lines.
            thread::sleep(Duration::from_millis(30));
            let stderr = lock(&stderr).iter().cloned().collect();
            events
                .unbounded_send(ProcessEvent::Exited { code, stderr })
                .ok();
        })?;
    Ok(())
}

/// A random version 4 UUID (the CLI echoes it in lifecycle frames and results).
pub fn new_uuid() -> String {
    let mut bytes = [0u8; 16];
    let filled = File::open("/dev/urandom")
        .and_then(|mut random| random.read_exact(&mut bytes))
        .is_ok();
    if !filled {
        // No /dev/urandom: the time and the process id are unique enough for message ids.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|time| time.as_nanos())
            .unwrap_or(0);
        bytes = (nanos ^ ((std::process::id() as u128) << 64)).to_le_bytes();
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuids_are_version_4() {
        let uuid = new_uuid();
        assert_eq!(uuid.len(), 36);
        assert_eq!(&uuid[14..15], "4");
        assert!(matches!(&uuid[19..20], "8" | "9" | "a" | "b"));
        assert_ne!(new_uuid(), uuid);
    }
}
