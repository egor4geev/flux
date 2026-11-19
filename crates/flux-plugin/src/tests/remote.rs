//! Claude for remote work and tasks between plugins (API 0.6, the bot's parts 1 and 3): the
//! probe's calls of `claude-remote` become the app's [`RemoteCall`]s and need the permission; a
//! watching instance hears [`broadcast`] and its own [`send_to`], and stops hearing them; a query
//! for tasks goes to the sources (never back to the asker) and their answers merge, each task
//! marked with its source; a task's action runs in the source's instance in the project's window,
//! and its answer — or none in time — comes back to the asker, a stranger's dropped.

use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use serde_json::Map;

use super::{Probe, probe_manifest, start, start_as, with_argument};
use crate::claude_remote::{
    RemoteCall, RemoteEvent, RemoteMode, RemoteReply, RemoteSession, SessionState, SystemEvent,
    broadcast, send_to,
};
use crate::registry::{PluginEntry, PluginSource, load_dir};
use crate::runtime::{HostCall, InstanceRole, MessageKind};

const COMMANDS: &[&str] = &[
    "remote:watch",
    "remote:unwatch",
    "remote:sessions",
    "remote:find",
    "remote:run",
    "remote:started",
];

/// The probe with `permissions` and, when `source`, `task-source = true`.
fn remote_probe(id: &str, permissions: &str, source: bool) -> Option<PluginEntry> {
    let entry = probe_manifest(
        id,
        &format!("project = \"none\"\n{permissions}"),
        COMMANDS,
        "",
    )?;
    if !source {
        return Some(entry);
    }
    let dir = entry.files.dir().unwrap().to_path_buf();
    let file = dir.join("flux-plugin.toml");
    let text = std::fs::read_to_string(&file).unwrap().replacen(
        "wasm = \"probe.wasm\"\n",
        "wasm = \"probe.wasm\"\ntask-source = true\n",
        1,
    );
    std::fs::write(&file, text).unwrap();
    Some(load_dir(&dir, PluginSource::Installed).unwrap())
}

impl Probe {
    /// The next call of `claude-remote`, answered with `reply`.
    fn remote_call(&mut self, reply: RemoteReply) -> RemoteCall {
        match self.next() {
            MessageKind::Call(HostCall::Claude {
                call,
                reply: sender,
            }) => {
                sender.send(reply).unwrap();
                call
            }
            other => panic!("expected a call of claude-remote, got {other:?}"),
        }
    }

    /// The next ask for a project's window (`tasks.run`): the project, and where the window's id
    /// goes.
    fn project_window(&mut self) -> (String, Sender<Result<u64, String>>) {
        match self.next() {
            MessageKind::Call(HostCall::ProjectWindow { project, reply }) => (project, reply),
            other => panic!("expected the ask for a project's window, got {other:?}"),
        }
    }
}

#[test]
fn claude_remote_needs_its_permission() {
    let Some(entry) = remote_probe("test.remote-denied", "", false) else {
        return;
    };
    let mut probe = start(entry, None, Map::new());
    assert_eq!(
        probe.run("remote:sessions"),
        "sessions failed: The plugin may not work with Claude's sessions: no `claude-remote = \
         true` in the permissions of flux-plugin.toml"
    );
    assert_eq!(
        probe.run("remote:find"),
        "find failed: The plugin may not ask for the user's tasks: no `tasks = true` in the \
         permissions of flux-plugin.toml"
    );
}

#[test]
fn a_watching_instance_hears_the_sessions() {
    let Some(entry) = remote_probe("test.remote-watch", "claude-remote = true", false) else {
        return;
    };
    let mut probe = start(entry, None, Map::new());
    probe.instance.run_command("remote:watch", super::palette());
    assert_eq!(
        probe.remote_call(RemoteReply::Done),
        RemoteCall::Watch(true)
    );
    assert_eq!(probe.notification(), "watching");
    assert!(crate::claude_remote::watching());

    broadcast(RemoteEvent::SessionGone("s1".into()));
    assert_eq!(probe.notification(), "gone s1");
    broadcast(RemoteEvent::System(SystemEvent::WillSleep));
    let system = probe.notification();
    assert!(
        system.starts_with("system ") && system.contains("WillSleep"),
        "{system}"
    );
    // A pult's action: only to its own plugin, never by `broadcast`.
    let action = RemoteEvent::Action {
        session: "s1".into(),
        action: "telegram".into(),
    };
    broadcast(action.clone());
    send_to("test.someone-else", action.clone());
    send_to("test.remote-watch", action);
    assert_eq!(probe.notification(), "action s1 telegram");

    probe
        .instance
        .run_command("remote:sessions", super::palette());
    let session = RemoteSession {
        id: "s1".into(),
        project: "/p".into(),
        title: "Fix".into(),
        state: SessionState::Idle,
        activity: String::new(),
        mode: RemoteMode::Ask,
        window: Some(1),
        updated: 0,
        request: None,
        unseen: true,
    };
    let sessions = RemoteReply::Sessions(vec![session]);
    assert_eq!(probe.remote_call(sessions), RemoteCall::Sessions);
    assert_eq!(probe.notification(), "sessions: s1");

    probe
        .instance
        .run_command("remote:unwatch", super::palette());
    assert_eq!(
        probe.remote_call(RemoteReply::Done),
        RemoteCall::Watch(false)
    );
    assert_eq!(probe.notification(), "unwatched");
    broadcast(RemoteEvent::SessionGone("s2".into()));
    probe.quiet(Duration::from_millis(300));
}

#[test]
fn a_query_for_tasks_goes_to_the_sources_and_merges() {
    let Some(source) = remote_probe("test.tasks-source", "", true) else {
        return;
    };
    // The asker gives tasks too: its own query never comes back to it.
    let Some(asker) = remote_probe("test.tasks-asker", "tasks = true", true) else {
        return;
    };
    let _source = start(source, None, Map::new());
    let mut asker = start(asker, None, Map::new());
    let started = Instant::now();
    // Other tests' sources may run meanwhile: theirs come too, in the order of their ids.
    let found = asker.run("remote:find");
    assert!(
        found.contains("P-1@test.tasks-source, P-2@test.tasks-source"),
        "{found}"
    );
    assert!(!found.contains("@test.tasks-asker"), "{found}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn a_run_goes_to_the_source_in_the_projects_window() {
    let (Some(source), Some(asker)) = (
        remote_probe("test.run-source", "", true),
        remote_probe("test.run-asker", "tasks = true", false),
    ) else {
        return;
    };
    // Neither its service nor its instance in another window does the work.
    let _service = start_as(source.clone(), InstanceRole::Service);
    let _elsewhere = start_as(source.clone(), InstanceRole::Window(9101));
    let mut asker = start(asker, None, Map::new());
    asker.instance.run_command(
        "remote:run",
        with_argument("test.run-source|P-1|work|/p/app"),
    );
    let (project, window) = asker.project_window();
    assert_eq!(project, "/p/app");
    // The window opens in the background: the source's instance there starts after the reply.
    window.send(Ok(9102)).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let _in_the_window = start_as(source, InstanceRole::Window(9102));
    assert_eq!(asker.notification(), "run: window-P-1 [work in /p/app]");

    // A task without a source, a source that doesn't run, a project that can't open, a source
    // that fails.
    asker
        .instance
        .run_command("remote:run", with_argument("|P-1|work|/p/app"));
    assert_eq!(
        asker.notification(),
        "run failed: The task has no source: take it from tasks.find"
    );
    asker
        .instance
        .run_command("remote:run", with_argument("test.nobody|P-1|work|/p/app"));
    assert_eq!(
        asker.notification(),
        "run failed: No plugin test.nobody gives tasks"
    );
    asker.instance.run_command(
        "remote:run",
        with_argument("test.run-source|P-1|work|/nowhere"),
    );
    let (_, window) = asker.project_window();
    window.send(Err("No such project folder".into())).unwrap();
    assert_eq!(asker.notification(), "run failed: No such project folder");
    asker.instance.run_command(
        "remote:run",
        with_argument("test.run-source|P-1|fail|/p/app"),
    );
    asker.project_window().1.send(Ok(9102)).unwrap();
    assert_eq!(asker.notification(), "run failed: can't run it");
}

#[test]
fn a_run_without_an_answer_fails_and_a_strangers_answer_is_dropped() {
    let (Some(source), Some(stranger), Some(asker)) = (
        remote_probe("test.run-silent", "", true),
        remote_probe("test.run-stranger", "", true),
        remote_probe("test.run-asker-2", "tasks = true", false),
    ) else {
        return;
    };
    crate::host::tasks::test_run_timeouts()
        .lock()
        .unwrap()
        .insert("test.run-silent".into(), Duration::from_secs(2));
    let mut source = start_as(source, InstanceRole::Window(9103));
    let mut stranger = start_as(stranger, InstanceRole::Window(9104));
    let mut asker = start(asker, None, Map::new());
    let started = Instant::now();
    asker.instance.run_command(
        "remote:run",
        with_argument("test.run-silent|P-2|silent|/p/app"),
    );
    asker.project_window().1.send(Ok(9103)).unwrap();
    let query = source.notification();
    let id = query.strip_prefix("run query ").expect(&query);
    // Another plugin answers the run: it isn't its, and the asker doesn't take it.
    stranger
        .instance
        .run_command("remote:started", with_argument(id));
    assert_eq!(stranger.notification(), "started sent");
    assert_eq!(
        asker.notification(),
        "run failed: Probe didn't answer in 2 seconds"
    );
    assert!(started.elapsed() >= Duration::from_secs(2));
    let log = stranger.log_text();
    assert!(
        log.contains(&format!(
            "tasks.started: the run {id} was asked of another plugin"
        )),
        "{log}"
    );
}
