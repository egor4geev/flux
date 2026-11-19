//! Plugins built for the API 0.3 and 0.4 run on this Flux (FluxGram part 1): the frozen probe
//! `tests/fixtures/probe-0.3.wasm` — the probe as the 0.3 SDK built it (Flux 0.4.1), never rebuilt
//! — with a manifest that says `api = "0.3"` or `"0.4"`; and, when `FLUX_TEST_YANDEX_DIR` names a
//! built copy of the Yandex Tracker plugin (0.3), that one.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::channel::mpsc::TryRecvError;
use serde_json::Map;

use super::{palette, plugin_folder, start};
use crate::api::events::{Event, UiEvent, UiInput};
use crate::api::tools::ToolCall;
use crate::registry::{PluginEntry, PluginSource, load_dir};
use crate::runtime::{HostCall, Instance, InstanceConfig, InstanceRole, MessageKind};

/// The probe of the API 0.3.
const PROBE_03: &[u8] = include_bytes!("../../tests/fixtures/probe-0.3.wasm");

fn probe_03(id: &str, api: &str) -> PluginEntry {
    plugin_folder(
        id,
        api,
        PROBE_03,
        "project = \"none\"",
        &["v3:form", "v3:picker"],
        "",
    )
}

#[test]
fn a_plugin_built_for_0_3_or_0_4_runs_and_hears_only_what_it_knows() {
    for api in ["0.3", "0.4"] {
        let mut probe = start(probe_03(&format!("test.v03-{api}"), api), None, Map::new());
        assert_eq!(probe.run("hello"), "hello");
        // 0.5's events don't reach it — and don't stop it.
        probe.instance.send_event(Event::ServiceStarted);
        probe
            .instance
            .send_event(Event::ServiceMessage((None, "hi".into())));
        probe.instance.send_event(Event::ToolCalled(ToolCall {
            id: 1,
            tool: "t".into(),
            arguments: "{}".into(),
        }));
        probe.instance.send_event(Event::TabClosed("w/k".into()));
        probe.instance.send_event(Event::Ui(UiInput {
            window: "w".into(),
            element: "chat".into(),
            event: UiEvent::LoadOlder,
        }));
        // A 0.3 call and its 0.3 answer go through.
        probe.instance.run_command("v3:form", palette());
        let id = match probe.next() {
            MessageKind::Call(HostCall::AskForm { id, form }) => {
                assert_eq!(form.title, "Open Task");
                id
            }
            other => panic!("expected a form, got {other:?}"),
        };
        assert_eq!(probe.notification(), format!("form: {id}"));
        probe.instance.send_event(Event::FormAnswered((
            id,
            Some(vec![
                ("branch".into(), "true".into()),
                ("name".into(), "TRACK-7".into()),
            ]),
        )));
        assert_eq!(
            probe.notification(),
            format!("form {id}: branch=true name=TRACK-7")
        );
        // A 0.3 picker goes through the 0.3 `picker` (0.5's items grew an avatar): its items
        // come without one.
        probe.instance.run_command("v3:picker", palette());
        match probe.next() {
            MessageKind::Call(HostCall::OpenPicker { spec, .. }) => {
                assert_eq!(spec.placeholder, "Find");
                assert_eq!(spec.items.len(), 1);
                assert_eq!(spec.items[0].key, "a");
                assert!(spec.items[0].avatar.is_none());
            }
            other => panic!("expected a picker, got {other:?}"),
        }
        // The context keeps the argument (0.3 has it).
        probe.instance.run_command(
            "context",
            crate::api::types::CommandContext {
                paths: vec!["/p/a.rs".into()],
                ..palette()
            },
        );
        assert_eq!(
            probe.notification(),
            "context: CommandSource::Palette [\"/p/a.rs\"]"
        );
    }
}

#[test]
fn a_plugin_built_for_0_3_stops_on_a_panic() {
    let mut probe = start(probe_03("test.v03-panic", "0.3"), None, Map::new());
    probe.instance.run_command("panic", palette());
    let (error, _) = probe.stopped();
    assert_eq!(error, "panicked: boom 42");
}

/// The Yandex Tracker plugin as published (API 0.3, the SDK pinned by its git revision), built
/// for `wasm32-wasip2` in `FLUX_TEST_YANDEX_DIR` (a copy of `flux-plugins/plugins/flux.yandex`):
/// it loads, activates and runs a command under this Flux.
#[test]
fn the_yandex_plugin_built_for_0_3_activates() {
    let Some(dir) = std::env::var_os("FLUX_TEST_YANDEX_DIR") else {
        // Not "skipped:" — CI reads that prefix as the probe plugin failing to build.
        eprintln!("not run: FLUX_TEST_YANDEX_DIR isn't set");
        return;
    };
    let entry = load_dir(std::path::Path::new(&dir), PluginSource::Installed).unwrap();
    assert_eq!(entry.manifest.api, "0.3");
    assert!(entry.problem.is_none(), "{:?}", entry.problem);
    let (messages, mut receiver) = futures::channel::mpsc::unbounded();
    let log = crate::log::PluginLog::memory();
    let instance = Instance::start(InstanceConfig {
        role: InstanceRole::Window(1),
        entry: Arc::new(entry),
        project_root: None,
        language: "ru".into(),
        settings: Map::new(),
        messages,
        log: log.clone(),
    });
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut calls = Vec::new();
    loop {
        match receiver.try_recv() {
            Ok(message) => match message.kind {
                MessageKind::Started => break,
                MessageKind::Stopped { error, details } => {
                    panic!("the plugin stopped: {error}\n{details}")
                }
                MessageKind::Call(call) => calls.push(format!("{call:?}")),
                MessageKind::Logged => {}
            },
            Err(TryRecvError::Closed) => panic!("the channel closed"),
            Err(TryRecvError::Empty) if Instant::now() > deadline => panic!("no start"),
            Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    // Its board's window shown, and 0.5's events, don't stop it.
    instance.send_event(Event::ServiceStarted);
    instance.send_event(Event::ToolWindowShown("board".into()));
    instance.send_event(Event::SettingsChanged);
    let quiet = Instant::now() + Duration::from_millis(500);
    while Instant::now() < quiet {
        match receiver.try_recv() {
            Ok(message) => match message.kind {
                MessageKind::Stopped { error, details } => {
                    panic!("the plugin stopped: {error}\n{details}")
                }
                MessageKind::Call(call) => calls.push(format!("{call:?}")),
                _ => {}
            },
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    let lines: Vec<String> = log.lines().into_iter().map(|line| line.text).collect();
    eprintln!(
        "yandex: activated; {} calls to the window ({}); log:\n{}",
        calls.len(),
        calls
            .iter()
            .map(|call| call.split([' ', '(', '{']).next().unwrap_or_default())
            .collect::<Vec<_>>()
            .join(", "),
        lines.join("\n")
    );
    drop(instance);
}
