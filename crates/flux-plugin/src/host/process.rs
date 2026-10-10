//! `process`: programs on this Mac, as the manifest's `processes` permission allows (by name, or
//! any). They run with the environment of the user's login shell — what a terminal gets, its `PATH`
//! above all: an app started from the Dock has a bare one — read once per Flux process
//! ([`login_env`]), in the project root by default. `run` waits on the plugin's thread (the wait
//! doesn't count toward its time limit); `spawn`'s reader threads post `process-output` and
//! `process-exited` events into the plugin's queue. Each program has a process group of its own,
//! so ending it ends what it started too; the plugin's programs end when it stops ([`Processes`]).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use super::window::missing;
use crate::api::bindings::flux::plugin::process;
use crate::api::events::Event;
use crate::host::HostState;
use crate::log::Level;
use crate::runtime::{EventSender, State};

/// `run`'s time limit when the plugin gives none.
const RUN_TIMEOUT: Duration = Duration::from_secs(60);
/// The most of a program's stdout (and of its stderr) `run` keeps; the rest is read and dropped.
const MAX_OUTPUT: usize = 64 << 20;
/// The largest piece of output in one `process-output` event.
const CHUNK: usize = 64 * 1024;
/// After SIGTERM a program has this long before SIGKILL.
const KILL_GRACE: Duration = Duration::from_secs(2);
/// How long the end of a program waits for its output to drain: a program it left running may keep
/// the pipes open.
const DRAIN: Duration = Duration::from_secs(2);
/// How long the login shell may take to tell its environment.
const SHELL_TIMEOUT: Duration = Duration::from_secs(5);
/// How often a waiting `run` looks whether the plugin is stopping.
const POLL: Duration = Duration::from_millis(50);
/// The `PATH` when the login shell can't tell its own.
const DEFAULT_PATH: &str = "/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/bin:/bin:\
                            /usr/sbin:/sbin";
/// Marks the login shell's environment in its output (a profile may print its own lines).
const MARK: &str = "__FLUX_LOGIN_ENV__";

/// The plugin's started programs, by id: ended when the plugin stops (the store goes) or the
/// project root changes (a new store).
#[derive(Default)]
pub(crate) struct Processes {
    running: Arc<Mutex<HashMap<u64, Started>>>,
}

/// A started program.
struct Started {
    /// Its process group: the program's pid.
    group: i32,
    /// Its input, written by a thread of its own; none once closed.
    input: Option<Sender<Vec<u8>>>,
    /// Set once the program has ended (and was reaped): its group isn't signalled any more.
    ended: Arc<AtomicBool>,
}

impl Processes {
    /// Reads the login shell's environment in the background, so that the plugin's first program
    /// doesn't wait for it.
    pub(crate) fn prepare_environment(&self) {
        let _ = thread::Builder::new()
            .name("plugin login environment".into())
            .spawn(|| {
                login_env();
            });
    }
}

impl Drop for Processes {
    fn drop(&mut self) {
        let running = std::mem::take(&mut *self.running.lock().unwrap());
        for (_, started) in running {
            terminate(started.group, started.ended);
        }
    }
}

impl process::Host for State {
    fn run(
        &mut self,
        command: process::Command,
        stdin: Option<Vec<u8>>,
        timeout_ms: Option<u32>,
    ) -> Result<process::Output, String> {
        let started = Instant::now();
        let timeout = timeout_ms.map_or(RUN_TIMEOUT, |ms| Duration::from_millis(ms.into()));
        let result = self.host.run_program(&command, stdin, timeout);
        self.host.waited += started.elapsed();
        result
    }

    fn spawn(&mut self, command: process::Command) -> Result<u64, String> {
        let started = Instant::now();
        let result = self.host.spawn_program(&command);
        // The first program may have waited for the login shell's environment.
        self.host.waited += started.elapsed();
        result
    }

    fn write(&mut self, process: u64, bytes: Vec<u8>) -> Result<(), String> {
        let running = self.host.processes.running.lock().unwrap();
        let input = running
            .get(&process)
            .ok_or_else(|| no_program(process))?
            .input
            .as_ref()
            .ok_or("The program's input is closed")?;
        input
            .send(bytes)
            .map_err(|_| "The program doesn't read its input any more".to_string())
    }

    fn close_stdin(&mut self, process: u64) {
        if let Some(started) = self
            .host
            .processes
            .running
            .lock()
            .unwrap()
            .get_mut(&process)
        {
            started.input = None;
        }
    }

    fn kill(&mut self, process: u64) {
        let running = self.host.processes.running.lock().unwrap();
        if let Some(started) = running.get(&process) {
            terminate(started.group, started.ended.clone());
        }
    }
}

fn no_program(process: u64) -> String {
    format!("No program {process}: it has ended, or it isn't the plugin's")
}

impl HostState {
    /// The program's command, as the permissions allow: found in the login shell's `PATH`, in its
    /// folder (the project root by default), with the login shell's environment and the plugin's
    /// additions, in a process group of its own, with all three pipes.
    fn command(&self, command: &process::Command) -> Result<Command, String> {
        let permissions = &self.entry.manifest.permissions;
        if !permissions.allows_program(&command.program) {
            let name = Path::new(&command.program)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| command.program.clone());
            return Err(missing(
                &format!("run {name}"),
                &format!("processes = [\"{name}\"]"),
            ));
        }
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let base = self
            .root
            .clone()
            .or(home)
            .unwrap_or_else(|| PathBuf::from("/"));
        let cwd = match &command.cwd {
            Some(cwd) => base.join(cwd),
            None => base,
        };
        if !cwd.is_dir() {
            return Err(format!("The folder {} isn't there", cwd.display()));
        }
        let env = login_env();
        let path = env
            .iter()
            .find(|(name, _)| name == "PATH")
            .map_or(DEFAULT_PATH, |(_, value)| value.as_str());
        let program = find_program(&command.program, path, &cwd)?;
        let mut process = Command::new(&program);
        process
            .args(&command.args)
            .current_dir(&cwd)
            .envs(env.iter().map(|(name, value)| (name, value)))
            .envs(command.env.iter().map(|(name, value)| (name, value)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        self.log.write(
            Level::Info,
            &format!("Runs {} in {}", shown(command), cwd.display()),
        );
        Ok(process)
    }

    /// `run`: the program to its end, its output, or why not.
    fn run_program(
        &mut self,
        command: &process::Command,
        stdin: Option<Vec<u8>>,
        timeout: Duration,
    ) -> Result<process::Output, String> {
        let mut child = self
            .command(command)?
            .spawn()
            .map_err(|err| format!("Couldn't start {}: {err}", command.program))?;
        let group = child.id() as i32;
        let ended = Arc::new(AtomicBool::new(false));
        if let Some(mut input) = child.stdin.take() {
            // Written by a thread: a program that doesn't read its input won't block the plugin.
            let bytes = stdin.unwrap_or_default();
            thread::spawn(move || {
                let _ = input.write_all(&bytes);
            });
        }
        let stdout = Capture::start(child.stdout.take());
        let stderr = Capture::start(child.stderr.take());
        let (status_tx, status_rx) = mpsc::channel();
        let reaped = ended.clone();
        thread::spawn(move || {
            let status = child.wait();
            reaped.store(true, Ordering::SeqCst);
            let _ = status_tx.send(status);
        });
        let deadline = Instant::now() + timeout;
        let status = loop {
            match status_rx.recv_timeout(POLL) {
                Ok(status) => {
                    break status.map_err(|err| format!("{} failed: {err}", command.program))?;
                }
                Err(RecvTimeoutError::Timeout) if self.cancel.load(Ordering::Relaxed) => {
                    terminate(group, ended);
                    return Err("The plugin is stopping".into());
                }
                Err(RecvTimeoutError::Timeout) if Instant::now() >= deadline => {
                    terminate(group, ended);
                    return Err(format!(
                        "{} didn't finish in {}; it was stopped",
                        command.program,
                        seconds(timeout)
                    ));
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(format!("{} was lost", command.program));
                }
            }
        };
        let drained = Instant::now() + DRAIN;
        let stdout = stdout.finish(drained);
        let stderr = stderr.finish(drained);
        if stdout.1 || stderr.1 {
            self.warn(&format!(
                "{}: the output was longer than {} MB; the rest was dropped",
                command.program,
                MAX_OUTPUT >> 20
            ));
        }
        Ok(process::Output {
            exit_code: exit_code(&status),
            stdout: stdout.0,
            stderr: stderr.0,
        })
    }

    /// `spawn`: starts the program; its output and its end come as events.
    fn spawn_program(&mut self, command: &process::Command) -> Result<u64, String> {
        let mut child = self
            .command(command)?
            .spawn()
            .map_err(|err| format!("Couldn't start {}: {err}", command.program))?;
        let id = self.next_id();
        let group = child.id() as i32;
        let ended = Arc::new(AtomicBool::new(false));
        let input = child.stdin.take().map(|mut pipe| {
            let (sender, bytes) = mpsc::channel::<Vec<u8>>();
            // Ends when the input is closed (the sender goes) or the program stops reading.
            thread::spawn(move || {
                for bytes in bytes {
                    if pipe.write_all(&bytes).and_then(|()| pipe.flush()).is_err() {
                        break;
                    }
                }
            });
            sender
        });
        let (drained_tx, drained) = mpsc::channel();
        let pipes: [(Option<Box<dyn Read + Send>>, process::OutputChannel); 2] = [
            (
                child
                    .stdout
                    .take()
                    .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
                process::OutputChannel::Stdout,
            ),
            (
                child
                    .stderr
                    .take()
                    .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
                process::OutputChannel::Stderr,
            ),
        ];
        for (pipe, channel) in pipes {
            let Some(pipe) = pipe else { continue };
            let events = self.events.clone();
            let drained_tx = drained_tx.clone();
            thread::spawn(move || {
                forward(pipe, id, channel, &events);
                let _ = drained_tx.send(());
            });
        }
        drop(drained_tx);
        // Known before the waiter can forget it: a program that ends at once is forgotten after.
        self.processes.running.lock().unwrap().insert(
            id,
            Started {
                group,
                input,
                ended: ended.clone(),
            },
        );
        let running = self.processes.running.clone();
        let events = self.events.clone();
        thread::spawn(move || {
            let status = child.wait();
            ended.store(true, Ordering::SeqCst);
            // The output first: both pipes reach their end (or a program it left behind keeps
            // them open, and the end comes anyway).
            wait_drained(&drained, Instant::now() + DRAIN);
            running.lock().unwrap().remove(&id);
            let code = status.ok().and_then(|status| exit_code(&status));
            events.send(Event::ProcessExited((id, code)));
        });
        Ok(id)
    }
}

/// A program's output to the plugin, a chunk per event, until the pipe ends or the plugin is gone.
fn forward(
    mut pipe: Box<dyn Read + Send>,
    id: u64,
    channel: process::OutputChannel,
    events: &EventSender,
) {
    let mut buffer = vec![0; CHUNK];
    loop {
        match pipe.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                let output = process::ProcessOutput {
                    process: id,
                    channel,
                    bytes: buffer[..read].to_vec(),
                };
                if !events.send(Event::ProcessOutput(output)) {
                    break;
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
}

/// Waits for both pipes' readers to end, until `until`.
fn wait_drained(drained: &Receiver<()>, until: Instant) {
    for _ in 0..2 {
        let left = until.saturating_duration_since(Instant::now());
        if drained.recv_timeout(left).is_err() {
            break;
        }
    }
}

/// One of `run`'s pipes, read to its end by a thread of its own.
struct Capture {
    output: Arc<Mutex<(Vec<u8>, bool)>>,
    done: Option<Receiver<()>>,
}

impl Capture {
    fn start(pipe: Option<impl Read + Send + 'static>) -> Capture {
        let output = Arc::new(Mutex::new((Vec::new(), false)));
        let Some(mut pipe) = pipe else {
            return Capture { output, done: None };
        };
        let (done_tx, done) = mpsc::channel();
        let shared = output.clone();
        thread::spawn(move || {
            let mut buffer = vec![0; CHUNK];
            loop {
                match pipe.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        let mut output = shared.lock().unwrap();
                        let room = MAX_OUTPUT.saturating_sub(output.0.len());
                        output.0.extend_from_slice(&buffer[..read.min(room)]);
                        output.1 |= read > room;
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            let _ = done_tx.send(());
        });
        Capture {
            output,
            done: Some(done),
        }
    }

    /// What was read by `until` (the pipe's end, normally), and whether some was dropped.
    fn finish(self, until: Instant) -> (Vec<u8>, bool) {
        if let Some(done) = &self.done {
            let _ = done.recv_timeout(until.saturating_duration_since(Instant::now()));
        }
        std::mem::take(&mut *self.output.lock().unwrap())
    }
}

/// Ends a program and what it started (its process group): SIGTERM now, SIGKILL if it still runs
/// after [`KILL_GRACE`] — on a thread of its own: nobody waits for it.
fn terminate(group: i32, ended: Arc<AtomicBool>) {
    if ended.load(Ordering::SeqCst) || group <= 0 {
        return;
    }
    signal(group, libc::SIGTERM);
    let _ = thread::Builder::new()
        .name("plugin program kill".into())
        .spawn(move || {
            thread::sleep(KILL_GRACE);
            if !ended.load(Ordering::SeqCst) {
                signal(group, libc::SIGKILL);
            }
        });
}

fn signal(group: i32, signal: libc::c_int) {
    // SAFETY: `kill` only sends a signal; a negative pid is the process group of a program this
    // plugin started (a group of its own), not reaped yet.
    unsafe {
        libc::kill(-group, signal);
    }
}

/// The exit code; none when a signal ended the program.
fn exit_code(status: &ExitStatus) -> Option<i32> {
    status.code()
}

/// The program as the log shows it: its name and arguments, cut short.
fn shown(command: &process::Command) -> String {
    let mut text = command.program.clone();
    for arg in &command.args {
        text.push(' ');
        text.push_str(arg);
    }
    if text.chars().count() > 200 {
        text = text.chars().take(200).collect::<String>() + "…";
    }
    text
}

fn seconds(duration: Duration) -> String {
    if duration < Duration::from_secs(1) {
        format!("{} ms", duration.as_millis())
    } else {
        format!("{} s", duration.as_secs())
    }
}

/// The program to run: a path (with a slash) as it is, relative to `cwd`; a name looked up in
/// `path`.
fn find_program(program: &str, path: &str, cwd: &Path) -> Result<PathBuf, String> {
    if program.contains('/') {
        let file = cwd.join(program);
        return if is_executable(&file) {
            Ok(file)
        } else {
            Err(format!("{program} isn't a program"))
        };
    }
    path.split(':')
        .filter(|dir| !dir.is_empty())
        .map(|dir| Path::new(dir).join(program))
        .find(|file| is_executable(file))
        .ok_or_else(|| format!("{program} isn't found (PATH: {path})"))
}

fn is_executable(file: &Path) -> bool {
    std::fs::metadata(file)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// The environment of the user's login shell, as a terminal of theirs gets it: read once per Flux
/// process — an interactive login shell first (a `.zshrc` sets `PATH` too), a non-interactive
/// one if that fails; Flux's own environment with a usual `PATH` if both do.
pub(crate) fn login_env() -> &'static [(String, String)] {
    static ENV: OnceLock<Vec<(String, String)>> = OnceLock::new();
    ENV.get_or_init(|| {
        let shell = std::env::var("SHELL")
            .ok()
            .filter(|shell| shell.starts_with('/'))
            .unwrap_or_else(|| "/bin/zsh".into());
        shell_env(&shell, &["-i", "-l"])
            .or_else(|| shell_env(&shell, &["-l"]))
            .unwrap_or_else(|| {
                let mut env: Vec<(String, String)> = std::env::vars()
                    .filter(|(name, _)| name != "PATH")
                    .collect();
                env.push(("PATH".into(), DEFAULT_PATH.into()));
                env
            })
    })
}

/// The environment `shell` with `flags` prints, if it does within [`SHELL_TIMEOUT`].
fn shell_env(shell: &str, flags: &[&str]) -> Option<Vec<(String, String)>> {
    let script = format!("printf '%s' '{MARK}'; /usr/bin/env -0; printf '%s' '{MARK}'");
    let mut child = Command::new(shell)
        .args(flags)
        .arg("-c")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .ok()?;
    let group = child.id() as i32;
    let output = Capture::start(child.stdout.take());
    let (status_tx, status_rx) = mpsc::channel();
    let ended = Arc::new(AtomicBool::new(false));
    let reaped = ended.clone();
    thread::spawn(move || {
        let status = child.wait();
        reaped.store(true, Ordering::SeqCst);
        let _ = status_tx.send(status);
    });
    if status_rx.recv_timeout(SHELL_TIMEOUT).is_err() {
        terminate(group, ended);
        return None;
    }
    let (output, _) = output.finish(Instant::now() + Duration::from_millis(500));
    parse_env(&output)
}

/// The variables between the marks of [`shell_env`]'s output; the shell's own bookkeeping
/// (`PWD`, `SHLVL`, `_`…) is left out.
fn parse_env(output: &[u8]) -> Option<Vec<(String, String)>> {
    let text = String::from_utf8_lossy(output);
    let start = text.find(MARK)? + MARK.len();
    let end = start + text[start..].find(MARK)?;
    let env: Vec<(String, String)> = text[start..end]
        .split('\0')
        .filter_map(|line| line.split_once('='))
        .filter(|(name, _)| {
            !matches!(*name, "PWD" | "OLDPWD" | "SHLVL" | "_" | "") && !name.contains('\n')
        })
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    env.iter().any(|(name, _)| name == "PATH").then_some(env)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programs_are_found_in_the_path() {
        let dir = crate::tests::temp_dir("find-program");
        let program = dir.join("tool");
        std::fs::write(&program, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(dir.join("text"), "").unwrap();
        let path = format!("/nowhere:{}", dir.display());
        assert_eq!(
            find_program("tool", &path, Path::new("/")).unwrap(),
            program
        );
        assert!(find_program("text", &path, Path::new("/")).is_err());
        assert!(
            find_program("missing", &path, Path::new("/"))
                .unwrap_err()
                .contains("PATH")
        );
        assert_eq!(
            find_program("./tool", "", &dir).unwrap(),
            dir.join("./tool")
        );
        assert_eq!(
            find_program(&program.to_string_lossy(), "", Path::new("/")).unwrap(),
            program
        );
    }

    #[test]
    fn the_login_environment_is_parsed_between_the_marks() {
        let output = format!(
            "Welcome!\n{MARK}PATH=/opt/homebrew/bin:/usr/bin\0HOME=/Users/me\0SHLVL=2\0X=a=b\0{MARK}"
        );
        let env = parse_env(output.as_bytes()).unwrap();
        assert_eq!(
            env,
            [
                ("PATH".to_string(), "/opt/homebrew/bin:/usr/bin".to_string()),
                ("HOME".to_string(), "/Users/me".to_string()),
                ("X".to_string(), "a=b".to_string()),
            ]
        );
        assert!(parse_env(b"no marks").is_none());
        assert!(parse_env(format!("{MARK}HOME=/x\0{MARK}").as_bytes()).is_none());
    }

    #[test]
    fn the_login_environment_has_a_path() {
        let env = login_env();
        let path = env.iter().find(|(name, _)| name == "PATH").unwrap();
        assert!(path.1.contains("/usr/bin"), "{}", path.1);
    }
}
