//! Claude for remote work and tasks between plugins (API 0.6, the bot's part 1): the probe's calls
//! of `claude-remote` become the app's [`RemoteCall`]s and need the permission; a watching
//! instance hears [`broadcast`] and its own [`send_to`], and stops hearing them; a query for
//! tasks goes to the sources (never back to the asker) and their answers merge.

use std::time::{Duration, Instant};

use serde_json::Map;

use super::{Probe, probe_manifest, start};
use crate::claude_remote::{
    RemoteCall, RemoteEvent, RemoteMode, RemoteReply, RemoteSession, SessionState, SystemEvent,
    broadcast, send_to,
};
use crate::registry::{PluginEntry, PluginSource, load_dir};
use crate::runtime::{HostCall, MessageKind};

const COMMANDS: &[&str] = &[
    "remote:watch",
    "remote:unwatch",
    "remote:sessions",
    "remote:find",
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
    assert_eq!(asker.run("remote:find"), "found: P-1, P-2");
    assert!(started.elapsed() < Duration::from_secs(5));
}
