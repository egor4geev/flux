//! Plugins built for the API 0.2 run on this Flux (part 10.3): the frozen probe
//! `tests/fixtures/probe-0.2.wasm` — the probe as the 0.2 SDK built it, never rebuilt — with a
//! manifest that says `api = "0.2"`.

use std::time::Duration;

use futures::channel::mpsc::TryRecvError;
use serde_json::{Map, json};

use super::{Probe, palette, plugin_folder, probe_manifest, start, temp_dir};
use crate::api::events::Event;
use crate::api::types::{CommandContext, CommandSource, EditorInfo};
use crate::registry::{PluginEntry, PluginSource, load_dir};
use crate::runtime::{EditorCall, EditorReply, HostCall, MessageKind};

/// The probe of the API 0.2.
const PROBE_02: &[u8] = include_bytes!("../../tests/fixtures/probe-0.2.wasm");

fn probe_02(id: &str, api: &str) -> PluginEntry {
    plugin_folder(id, api, PROBE_02, "project = \"none\"", &[], "")
}

#[test]
fn a_plugin_built_for_0_2_runs_its_commands() {
    let mut probe = start(probe_02("test.v02-hello", "0.2"), None, Map::new());
    assert_eq!(probe.run("hello"), "hello");
    assert_eq!(probe.run("context"), "context: CommandSource::Palette []");
    // A source 0.2 doesn't know comes as the palette; the argument stays behind.
    probe.instance.run_command(
        "context",
        CommandContext {
            source: CommandSource::Settings,
            editor: None,
            selections: Vec::new(),
            paths: vec!["/p/a.rs".into()],
            argument: Some("TRACK-1".into()),
        },
    );
    assert_eq!(
        probe.notification(),
        "context: CommandSource::Palette [\"/p/a.rs\"]"
    );
    probe.instance.run_command(
        "context",
        CommandContext {
            source: CommandSource::TreeMenu,
            ..palette()
        },
    );
    assert_eq!(probe.notification(), "context: CommandSource::TreeMenu []");
}

#[test]
fn a_plugin_built_for_0_2_asks_the_window() {
    let mut probe = start(probe_02("test.v02-echo", "0.2"), None, Map::new());
    probe.instance.run_command("echo", palette());
    let info = EditorInfo {
        id: 3,
        path: None,
        language: "Plain Text".into(),
        modified: true,
    };
    match probe.next() {
        MessageKind::Call(HostCall::Editor {
            call: EditorCall::Active,
            reply,
        }) => reply.send(EditorReply::Editor(Some(info))).unwrap(),
        other => panic!("expected an editor call, got {other:?}"),
    }
    match probe.next() {
        MessageKind::Call(HostCall::Editor {
            call: EditorCall::Text(3),
            reply,
        }) => reply.send(EditorReply::Text(Some("old".into()))).unwrap(),
        other => panic!("expected the text call, got {other:?}"),
    }
    assert_eq!(probe.notification(), "text: old");
}

#[test]
fn a_plugin_built_for_0_2_hears_only_what_it_knows() {
    let mut probe = start(probe_02("test.v02-events", "0.2"), None, Map::new());
    // 0.3's events don't reach it — and don't stop it.
    probe
        .instance
        .send_event(Event::FormAnswered((1, Some(vec![("a".into(), "b".into())]))));
    probe.instance.send_event(Event::PickerChosen((2, None)));
    probe.instance.send_event(Event::GitDone((3, Err("no".into()))));
    probe
        .instance
        .set_settings(Map::from_iter([("greeting".to_string(), json!("hey"))]));
    probe.instance.send_event(Event::SettingsChanged);
    assert_eq!(probe.notification(), "settings changed");
    assert_eq!(probe.run("setting"), "setting: hey");
    // Its settings, storage and translations work as before.
    probe.instance.run_command("store", palette());
    assert_eq!(probe.run("load"), "stored: hi there");
}

#[test]
fn a_plugin_built_for_0_2_stops_on_a_panic() {
    let mut probe = start(probe_02("test.v02-panic", "0.2"), None, Map::new());
    probe.instance.run_command("panic", palette());
    let (error, _) = probe.stopped();
    assert_eq!(error, "panicked: boom 42");
}

#[test]
fn the_manifest_must_name_the_api_the_component_is_built_for() {
    // A 0.2 component that says it is 0.3.
    let mut probe = start_stopped(probe_02("test.v02-says-03", "0.3"));
    let (error, _) = probe.stopped();
    assert_eq!(
        error,
        "The component is built for plugin API 0.2, but flux-plugin.toml says api = \"0.3\""
    );
    // A 0.3 component that says it is 0.2.
    let Some(entry) = probe_manifest("test.v03-says-02", "project = \"none\"", &[], "") else {
        return;
    };
    let dir = entry.files.dir().expect("a folder").to_path_buf();
    let manifest = std::fs::read_to_string(dir.join("flux-plugin.toml")).unwrap();
    std::fs::write(
        dir.join("flux-plugin.toml"),
        manifest.replace("api = \"0.3\"", "api = \"0.2\""),
    )
    .unwrap();
    let entry = load_dir(&dir, PluginSource::Installed).unwrap();
    let mut probe = start_stopped(entry);
    let (error, _) = probe.stopped();
    assert_eq!(
        error,
        "The component is built for plugin API 0.3, but flux-plugin.toml says api = \"0.2\""
    );
}

/// An instance that is expected to stop before it starts.
fn start_stopped(entry: PluginEntry) -> Probe {
    let (messages, receiver) = futures::channel::mpsc::unbounded();
    let log = crate::log::PluginLog::memory();
    let instance = crate::runtime::Instance::start(crate::runtime::InstanceConfig {
        entry: std::sync::Arc::new(entry),
        project_root: None,
        language: "en".into(),
        settings: Map::new(),
        messages,
        log: log.clone(),
    });
    Probe {
        instance,
        messages: receiver,
        log,
    }
}

#[test]
fn plugins_without_code_load_for_either_api_and_not_for_others() {
    for (api, works) in [("0.2", true), ("0.3", true), ("9.9", false)] {
        let dir = temp_dir("declarative-api");
        std::fs::write(
            dir.join("flux-plugin.toml"),
            format!("id = \"test.theme\"\nname = \"Theme\"\nversion = \"1.0.0\"\napi = \"{api}\"\n"),
        )
        .unwrap();
        let entry = load_dir(&dir, PluginSource::Installed).unwrap();
        if works {
            assert_eq!(entry.problem, None, "api {api}");
        } else {
            assert_eq!(
                entry.problem.as_deref(),
                Some("Built for plugin API 9.9; this Flux runs 0.2, 0.3")
            );
        }
    }
}

#[test]
fn a_plugin_built_for_0_2_without_new_events_stays_quiet() {
    let mut probe = start(probe_02("test.v02-quiet", "0.2"), None, Map::new());
    probe.instance.send_event(Event::LinkHovered((1, "p".into(), "TRACK-1".into())));
    probe.instance.send_event(Event::PickerQuery((1, "x".into())));
    std::thread::sleep(Duration::from_millis(100));
    loop {
        match probe.messages.try_recv() {
            Ok(message) => assert!(
                matches!(message.kind, MessageKind::Logged),
                "unexpected {:?}",
                message.kind
            ),
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Closed) => panic!("the instance stopped"),
        }
    }
    assert_eq!(probe.run("hello"), "hello");
}
