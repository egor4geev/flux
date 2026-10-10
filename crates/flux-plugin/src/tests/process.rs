//! The runtime against the probe: programs, timers, secrets, folders outside the project and the
//! time limit. The probe's commands are in `tests/fixtures/probe/src/processes.rs`;
//! `super::probe_with` makes a probe with the permissions and the commands a test needs.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use futures::channel::mpsc::TryRecvError;
use serde_json::{Map, json};

use super::{Probe, palette, probe_with, start, temp_dir};
use crate::registry::PluginEntry;
use crate::runtime::{HostCall, MessageKind};

/// The probe's commands of this part.
const COMMANDS: &[&str] = &[
    "proc:run-echo",
    "proc:run-stdin",
    "proc:run-exit",
    "proc:run-denied",
    "proc:run-missing",
    "proc:run-timeout",
    "proc:run-sleep",
    "proc:run-cwd",
    "proc:run-env",
    "proc:spawn-cat",
    "proc:spawn-both",
    "proc:spawn-kill",
    "proc:spawn-long",
    "proc:spawn-denied",
    "proc:timer-after",
    "proc:timer-every",
    "proc:timer-cancel",
    "proc:folder-read",
    "proc:folder-read-other",
    "proc:folder-write",
    "proc:folder-write-readable",
    "proc:secret-set",
    "proc:secret-get",
    "proc:secret-delete",
];

/// The programs the probe may run.
const PROGRAMS: &str =
    "processes = [\"echo\", \"cat\", \"sh\", \"sleep\", \"pwd\", \"flux-no-such-program\"]";

fn proc_probe(id: &str, permissions: &str) -> Option<PluginEntry> {
    probe_with(id, permissions, COMMANDS)
}

/// Puts values into the plugin's storage before it starts (the probe reads its folders there).
fn store(id: &str, values: serde_json::Value) {
    let dir = crate::paths::data_dir(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("storage.json"), values.to_string()).unwrap();
}

impl Probe {
    /// No notification for `duration` (log lines don't count).
    fn quiet(&mut self, duration: Duration) {
        let until = Instant::now() + duration;
        while Instant::now() < until {
            match self.messages.try_recv() {
                Ok(message) => match message.kind {
                    MessageKind::Logged => {}
                    MessageKind::Call(HostCall::Notify { notification, .. }) => {
                        panic!("unexpected notification: {}", notification.title)
                    }
                    other => panic!("unexpected message: {other:?}"),
                },
                Err(TryRecvError::Closed) => panic!("the instance's channel is closed"),
                Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
    }
}

/// Whether a program with this command line runs (`pgrep -f`).
fn running(pattern: &str) -> bool {
    Command::new("pgrep")
        .args(["-f", pattern])
        .output()
        .is_ok_and(|output| output.status.success())
}

#[test]
fn runs_programs_to_their_end() {
    let root = temp_dir("proc-root");
    let Some(entry) = proc_probe("test.proc-run", &format!("project = \"read\"\n{PROGRAMS}"))
    else {
        return;
    };
    let mut probe = start(entry, Some(root.clone()), Map::new());
    assert_eq!(probe.run("proc:run-echo"), "run: Some(0) hello|");
    assert_eq!(probe.run("proc:run-stdin"), "stdin: piped input");
    assert_eq!(probe.run("proc:run-exit"), "exit: Some(3) oops");
    let denied = probe.run("proc:run-denied");
    assert!(
        denied.contains("may not run ls") && denied.contains("processes = [\"ls\"]"),
        "{denied}"
    );
    let missing = probe.run("proc:run-missing");
    assert!(missing.contains("isn't found"), "{missing}");
    let started = Instant::now();
    let timeout = probe.run("proc:run-timeout");
    assert!(timeout.contains("didn't finish in 300 ms"), "{timeout}");
    assert!(started.elapsed() < Duration::from_secs(3));
    let cwd = probe.run("proc:run-cwd");
    let root = std::fs::canonicalize(&root).unwrap();
    assert_eq!(cwd, format!("cwd: {}", root.display()));
    let env = probe.run("proc:run-env");
    assert!(env.starts_with("env: 42 "), "{env}");
    assert!(env.contains("/usr/bin"), "{env}");
    let log = probe.log_text();
    assert!(log.contains("Runs echo hello in"), "{log}");
}

#[test]
fn started_programs_report_their_output_and_their_end() {
    let Some(entry) = proc_probe("test.proc-spawn", PROGRAMS) else {
        return;
    };
    let mut probe = start(entry, None, Map::new());
    assert_eq!(
        probe.run("proc:spawn-cat"),
        "cat: out=abc err= code=Some(0)"
    );
    assert_eq!(
        probe.run("proc:spawn-both"),
        "both: out=out err=err code=Some(5)"
    );
    let started = Instant::now();
    assert_eq!(probe.run("proc:spawn-kill"), "killed: out= err= code=None");
    assert!(started.elapsed() < Duration::from_secs(2));
    let denied = probe.run("proc:spawn-denied");
    assert!(denied.contains("may not run ls"), "{denied}");
}

#[test]
fn programs_end_when_the_plugin_stops() {
    let Some(entry) = proc_probe("test.proc-stop", PROGRAMS) else {
        return;
    };
    let mut probe = start(entry, None, Map::new());
    assert_eq!(probe.run("proc:spawn-long"), "spawned");
    assert!(running("sleep 31.517"));
    drop(probe);
    let until = Instant::now() + Duration::from_secs(5);
    while running("sleep 31.517") {
        assert!(Instant::now() < until, "the program outlived its plugin");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn waiting_for_a_program_does_not_count_toward_the_limit() {
    let Some(entry) = proc_probe("test.proc-limit", PROGRAMS) else {
        return;
    };
    crate::runtime::test_limits()
        .lock()
        .unwrap()
        .insert("test.proc-limit".into(), Duration::from_millis(500));
    let mut probe = start(entry, None, Map::new());
    assert_eq!(probe.run("proc:run-sleep"), "slept: Some(0)");
    probe.instance.run_command("spin", palette());
    let (error, _) = probe.stopped();
    assert_eq!(error, "took longer than 500 ms");
}

#[test]
fn timers_fire_and_cancel() {
    let Some(entry) = proc_probe("test.proc-timers", "") else {
        return;
    };
    let mut probe = start(entry, None, Map::new());
    assert_eq!(probe.run("proc:timer-after"), "timer: after");
    let started = Instant::now();
    assert_eq!(probe.run("proc:timer-every"), "timer: every 3");
    assert!(started.elapsed() >= Duration::from_millis(290));
    probe.quiet(Duration::from_millis(400));
    assert_eq!(probe.run("proc:timer-cancel"), "timer: sentinel");
}

#[test]
fn folders_outside_the_project_follow_their_permissions() {
    let readable = temp_dir("proc-readable");
    let writable = temp_dir("proc-writable");
    let other = temp_dir("proc-other");
    std::fs::write(readable.join("note.txt"), "outside").unwrap();
    std::fs::write(other.join("note.txt"), "secret").unwrap();
    let missing = Path::new("/nonexistent/flux-folder");
    let permissions = format!(
        "folders = [{{ path = \"{}\" }}, {{ path = \"{}\", access = \"write\" }}, \
         {{ path = \"{}\" }}]",
        readable.display(),
        writable.display(),
        missing.display()
    );
    let Some(entry) = proc_probe("test.proc-folders", &permissions) else {
        return;
    };
    store(
        "test.proc-folders",
        json!({
            "readable": readable.to_string_lossy(),
            "writable": writable.to_string_lossy(),
            "other": other.to_string_lossy(),
        }),
    );
    let mut probe = start(entry, None, Map::new());
    assert_eq!(probe.run("proc:folder-read"), "readable: outside");
    assert_eq!(probe.run("proc:folder-read-other"), "other: can't read");
    assert_eq!(probe.run("proc:folder-write"), "writable write: ok");
    assert_eq!(
        std::fs::read_to_string(writable.join("out.txt")).unwrap(),
        "written"
    );
    assert_eq!(
        probe.run("proc:folder-write-readable"),
        "readable write: can't"
    );
    assert!(!readable.join("out.txt").exists());
    let log = probe.log_text();
    assert!(
        log.contains("Couldn't open the folder /nonexistent/flux-folder"),
        "{log}"
    );
}

/// Uses the login keychain of the user who runs the tests (no prompt: the test binary creates,
/// reads and deletes its own item), so it runs only when asked:
/// `cargo test -p flux-plugin -- --ignored secrets`.
#[test]
#[ignore]
fn secrets_are_kept_in_the_keychain() {
    let id = "test.flux-secrets-probe";
    let service = crate::keychain::service(id);
    crate::keychain::delete_all(&service).unwrap();
    let Some(entry) = proc_probe(id, "") else {
        return;
    };
    let mut probe = start(entry, None, Map::new());
    assert_eq!(probe.run("proc:secret-get"), "secret: None");
    assert_eq!(probe.run("proc:secret-set"), "secret set");
    assert_eq!(probe.run("proc:secret-get"), "secret: Some(\"s3cret\")");
    assert_eq!(probe.run("proc:secret-delete"), "secret deleted");
    assert_eq!(probe.run("proc:secret-get"), "secret: None");
    // Uninstalling removes every secret of the plugin.
    assert_eq!(probe.run("proc:secret-set"), "secret set");
    crate::keychain::set(&service, "other", "x").unwrap();
    crate::keychain::delete_all(&service).unwrap();
    assert_eq!(probe.run("proc:secret-get"), "secret: None");
    assert_eq!(crate::keychain::get(&service, "other").unwrap(), None);
}
