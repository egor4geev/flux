//! The probe's commands for programs (`process`), timers, secrets, folders outside the project and
//! the time limit: `proc:<name>`; they report through notifications, as the other commands (their
//! titles are what the tests check). A test passes folders through the storage.

use std::cell::RefCell;
use std::collections::HashMap;

use flux_plugin_api::host::process::{self, Command, OutputChannel};
use flux_plugin_api::host::{secrets, storage, timers};
use flux_plugin_api::{Event, notify};

/// A notification: what the tests read.
fn say(text: &str) {
    notify::info(text);
}

/// What the background work has brought so far.
#[derive(Default)]
struct Watch {
    /// A started program's stdout and stderr, by id.
    output: HashMap<u64, (Vec<u8>, Vec<u8>)>,
    /// What to say when a program ends, by id.
    on_exit: HashMap<u64, &'static str>,
    /// The ticks of the periodic timer, and its id.
    ticks: u32,
    every: Option<u64>,
    /// The timer that must not fire, and the one that says it didn't.
    cancelled: Option<u64>,
    sentinel: Option<u64>,
    after: Option<u64>,
}

thread_local! {
    static WATCH: RefCell<Watch> = RefCell::new(Watch::default());
}

fn command(program: &str, args: &[&str]) -> Command {
    Command {
        program: program.into(),
        args: args.iter().map(|arg| arg.to_string()).collect(),
        cwd: None,
        env: Vec::new(),
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Starts a program and says `what` when it ends.
fn spawn(command: &Command, what: &'static str) -> Option<u64> {
    match process::spawn(command) {
        Ok(id) => {
            WATCH.with(|watch| {
                let mut watch = watch.borrow_mut();
                watch.output.insert(id, Default::default());
                watch.on_exit.insert(id, what);
            });
            Some(id)
        }
        Err(err) => {
            say(&format!("{what}: can't spawn: {err}"));
            None
        }
    }
}

pub fn run(command_name: &str) {
    match command_name {
        "run-echo" => match process::run(&command("echo", &["hello"]), None, None) {
            Ok(output) => say(&format!(
                "run: {:?} {}|{}",
                output.exit_code,
                text(&output.stdout).trim(),
                text(&output.stderr).trim()
            )),
            Err(err) => say(&format!("run failed: {err}")),
        },
        "run-stdin" => {
            match process::run(&command("cat", &[]), Some(b"piped input".as_slice()), None) {
                Ok(output) => say(&format!("stdin: {}", text(&output.stdout))),
                Err(err) => say(&format!("stdin failed: {err}")),
            }
        }
        "run-exit" => {
            let command = command("sh", &["-c", "echo oops >&2; exit 3"]);
            match process::run(&command, None, None) {
                Ok(output) => say(&format!(
                    "exit: {:?} {}",
                    output.exit_code,
                    text(&output.stderr).trim()
                )),
                Err(err) => say(&format!("exit failed: {err}")),
            }
        }
        "run-denied" => match process::run(&command("ls", &[]), None, None) {
            Ok(_) => say("denied: it ran"),
            Err(err) => say(&format!("denied: {err}")),
        },
        "run-missing" => match process::run(&command("flux-no-such-program", &[]), None, None) {
            Ok(_) => say("missing: it ran"),
            Err(err) => say(&format!("missing: {err}")),
        },
        "run-timeout" => match process::run(&command("sleep", &["5"]), None, Some(300)) {
            Ok(_) => say("timeout: it finished"),
            Err(err) => say(&format!("timeout: {err}")),
        },
        "run-sleep" => match process::run(&command("sleep", &["2"]), None, None) {
            Ok(output) => say(&format!("slept: {:?}", output.exit_code)),
            Err(err) => say(&format!("sleep failed: {err}")),
        },
        "run-cwd" => match process::run(&command("pwd", &[]), None, None) {
            Ok(output) => say(&format!("cwd: {}", text(&output.stdout).trim())),
            Err(err) => say(&format!("cwd failed: {err}")),
        },
        "run-env" => {
            let mut command = command(
                "sh",
                &["-c", "printf %s \"$FLUX_TEST\"; printf ' %s' \"$PATH\""],
            );
            command.env.push(("FLUX_TEST".into(), "42".into()));
            match process::run(&command, None, None) {
                Ok(output) => say(&format!("env: {}", text(&output.stdout))),
                Err(err) => say(&format!("env failed: {err}")),
            }
        }
        "spawn-cat" => {
            if let Some(id) = spawn(&command("cat", &[]), "cat") {
                let written = process::write(id, b"abc");
                process::close_stdin(id);
                if let Err(err) = written {
                    say(&format!("cat: can't write: {err}"));
                }
            }
        }
        "spawn-both" => {
            spawn(
                &command("sh", &["-c", "echo out; echo err >&2; exit 5"]),
                "both",
            );
        }
        "spawn-kill" => {
            if let Some(id) = spawn(&command("sleep", &["30"]), "killed") {
                process::kill(id);
            }
        }
        "spawn-long" => {
            if spawn(&command("sleep", &["31.517"]), "long").is_some() {
                say("spawned");
            }
        }
        "spawn-denied" => {
            if let Err(err) = process::spawn(&command("ls", &[])) {
                say(&format!("spawn denied: {err}"));
            }
        }
        "timer-after" => {
            let id = timers::after(50);
            WATCH.with(|watch| watch.borrow_mut().after = Some(id));
        }
        "timer-every" => {
            let id = timers::every(100);
            WATCH.with(|watch| {
                let mut watch = watch.borrow_mut();
                watch.ticks = 0;
                watch.every = Some(id);
            });
        }
        "timer-cancel" => {
            let cancelled = timers::after(150);
            timers::cancel(cancelled);
            let sentinel = timers::after(400);
            WATCH.with(|watch| {
                let mut watch = watch.borrow_mut();
                watch.cancelled = Some(cancelled);
                watch.sentinel = Some(sentinel);
            });
        }
        // The folders a test puts into the storage: `readable` (read access), `writable` (write
        // access), `other` (not in the permissions).
        "folder-read" => read_note("readable"),
        "folder-read-other" => read_note("other"),
        "folder-write" => write_file("writable"),
        "folder-write-readable" => write_file("readable"),
        "secret-set" => match secrets::set("token", Some("s3cret")) {
            Ok(()) => say("secret set"),
            Err(err) => say(&format!("secret set failed: {err}")),
        },
        "secret-get" => say(&format!("secret: {:?}", secrets::get("token"))),
        "secret-delete" => match secrets::set("token", None) {
            Ok(()) => say("secret deleted"),
            Err(err) => say(&format!("secret delete failed: {err}")),
        },
        _ => {}
    }
}

fn read_note(key: &str) {
    let folder = storage::get(key).unwrap_or_default();
    match std::fs::read_to_string(format!("{folder}/note.txt")) {
        Ok(text) => say(&format!("{key}: {}", text.trim())),
        Err(_) => say(&format!("{key}: can't read")),
    }
}

fn write_file(key: &str) {
    let folder = storage::get(key).unwrap_or_default();
    match std::fs::write(format!("{folder}/out.txt"), "written") {
        Ok(()) => say(&format!("{key} write: ok")),
        Err(_) => say(&format!("{key} write: can't")),
    }
}

/// Handles the events of programs and timers; true when the event was one of them.
pub fn on_event(event: &Event) -> bool {
    match event {
        Event::ProcessOutput(output) => {
            WATCH.with(|watch| {
                let mut watch = watch.borrow_mut();
                let (stdout, stderr) = watch.output.entry(output.process).or_default();
                match output.channel {
                    OutputChannel::Stdout => stdout.extend_from_slice(&output.bytes),
                    OutputChannel::Stderr => stderr.extend_from_slice(&output.bytes),
                }
            });
            true
        }
        Event::ProcessExited((id, code)) => {
            let (what, output) = WATCH.with(|watch| {
                let mut watch = watch.borrow_mut();
                (
                    watch.on_exit.remove(id),
                    watch.output.remove(id).unwrap_or_default(),
                )
            });
            if let Some(what) = what {
                say(&format!(
                    "{what}: out={} err={} code={code:?}",
                    text(&output.0).trim(),
                    text(&output.1).trim()
                ));
            }
            true
        }
        Event::Timer(id) => {
            let said = WATCH.with(|watch| {
                let mut watch = watch.borrow_mut();
                if watch.after == Some(*id) {
                    watch.after = None;
                    return Some("timer: after".to_string());
                }
                if watch.every == Some(*id) {
                    watch.ticks += 1;
                    if watch.ticks == 3 {
                        timers::cancel(*id);
                        return Some("timer: every 3".to_string());
                    }
                    if watch.ticks > 3 {
                        return Some(format!("timer: every {} after cancel", watch.ticks));
                    }
                    return None;
                }
                if watch.cancelled == Some(*id) {
                    return Some("timer: the cancelled one fired".to_string());
                }
                if watch.sentinel == Some(*id) {
                    return Some("timer: sentinel".to_string());
                }
                None
            });
            if let Some(said) = said {
                say(&said);
            }
            true
        }
        _ => false,
    }
}
