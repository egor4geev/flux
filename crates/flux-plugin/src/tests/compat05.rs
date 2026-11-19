//! Plugins built for the API 0.5 run on this Flux (the bot, part 1): the frozen probe
//! `tests/fixtures/probe-0.5.wasm` — the probe as the 0.5 SDK built it (Flux 0.5.0), never rebuilt
//! — with a manifest that says `api = "0.5"`.

use std::sync::Arc;

use serde_json::Map;

use super::{Probe, palette, plugin_folder, start};
use crate::api::claude_remote::{Session, SessionState, SystemEvent};
use crate::api::events::Event;
use crate::api::tasks::{Query, TaskQuery};
use crate::api::tools::ToolCall;
use crate::registry::PluginEntry;
use crate::runtime::{HostCall, Instance, InstanceConfig, InstanceRole, MessageKind};

/// The probe of the API 0.5.
const PROBE_05: &[u8] = include_bytes!("../../tests/fixtures/probe-0.5.wasm");

fn probe_05(id: &str, api: &str) -> PluginEntry {
    plugin_folder(
        id,
        api,
        PROBE_05,
        "project = \"none\"",
        &["svc:is-service", "svc:respond", "v3:form"],
        "",
    )
}

#[test]
fn a_plugin_built_for_0_5_runs_and_hears_only_what_it_knows() {
    let mut probe = start(probe_05("test.v05", "0.5"), None, Map::new());
    assert_eq!(probe.run("hello"), "hello");
    // 0.6's events don't reach it — and don't stop it.
    probe
        .instance
        .send_event(Event::SystemChanged(SystemEvent::WillSleep));
    probe.instance.send_event(Event::ClaudeSession(Session {
        id: "s1".into(),
        project: "/p".into(),
        title: String::new(),
        state: SessionState::Working,
        activity: String::new(),
        mode: crate::api::claude_remote::Mode::Ask,
        window: Some(1),
        updated: 0,
        request: None,
        unseen: false,
    }));
    probe.instance.send_event(Event::TaskQuery(TaskQuery {
        id: 1,
        query: Query::Mine,
    }));
    probe
        .instance
        .send_event(Event::ClaudeAction(("s1".into(), "telegram".into())));
    // 0.5's calls and events go through: the service, Claude's tools, a 0.3 form.
    assert_eq!(probe.run("svc:is-service"), "is-service: false");
    probe.instance.send_event(Event::ToolCalled(ToolCall {
        id: 4,
        tool: "reply".into(),
        arguments: "{}".into(),
    }));
    assert_eq!(probe.notification(), "tool-called 4 reply");
    probe.instance.run_command("v3:form", palette());
    match probe.next() {
        MessageKind::Call(HostCall::AskForm { form, .. }) => {
            assert_eq!(form.title, "Open Task")
        }
        other => panic!("expected a form, got {other:?}"),
    }
}

#[test]
fn a_component_must_be_built_for_the_api_its_manifest_names() {
    let (messages, receiver) = futures::channel::mpsc::unbounded();
    let log = crate::log::PluginLog::memory();
    let instance = Instance::start(InstanceConfig {
        role: InstanceRole::Window(1),
        entry: Arc::new(probe_05("test.v05-as-06", crate::API_VERSION)),
        project_root: None,
        language: "en".into(),
        settings: Map::new(),
        messages,
        log: log.clone(),
    });
    let mut probe = Probe {
        instance,
        messages: receiver,
        log,
    };
    let (error, _) = probe.stopped();
    assert_eq!(
        error,
        "The component is built for plugin API 0.5, but flux-plugin.toml says api = \"0.6\""
    );
}
