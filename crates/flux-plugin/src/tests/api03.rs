//! The API 0.3's new calls against the probe (part 10.3): what reaches the window, and what the
//! permissions and the manifest refuse. The probe's commands are in
//! `tests/fixtures/probe/src/api03.rs`.

use std::sync::mpsc::Sender;

use serde_json::{Map, json};

use super::{Probe, probe_manifest, start, temp_dir};
use crate::api::claude::{Attachment, SessionSpec};
use crate::api::contexts::ContextRef;
use crate::api::events::Event;
use crate::api::git::{Branch, CommitTemplate};
use crate::api::picker::PickerAnchor;
use crate::api::settings::SettingStatus;
use crate::api::ui::Tone;
use crate::runtime::{HostCall, MessageKind, WindowCall, WindowReply};

const COMMANDS: &[&str] = &[
    "v3:settings-set",
    "v3:settings-set-number",
    "v3:settings-set-status",
    "v3:settings-reset",
    "v3:settings-status",
    "v3:settings-status-wrong",
    "v3:form",
    "v3:picker",
    "v3:picker-unknown-anchor",
    "v3:picker-update",
    "v3:picker-close",
    "v3:branches",
    "v3:new-branch",
    "v3:checkout",
    "v3:commit-template",
    "v3:links",
    "v3:describe",
    "v3:claude",
    "v3:contexts-switch",
    "v3:contexts-remove",
    "v3:utc-offset",
    "v3:time-zone",
    "v3:open-settings",
];

/// A status bar item and a status line for the probe's calls.
const EXTRA: &str = "[[status-items]]\nid = \"clock\"\n\n[[settings]]\nkey = \
                     \"account-status\"\ntype = \"status\"\ntitle = \"Account\"\ndefault = \"Not \
                     signed in\"\n";

/// A running probe with `permissions`, in a project of its own.
fn v3_probe(id: &str, permissions: &str) -> Option<(Probe, String)> {
    let entry = probe_manifest(id, permissions, COMMANDS, EXTRA)?;
    let root = temp_dir(&format!("{id}-project"));
    let probe = start(entry, Some(root.clone()), Map::new());
    Some((probe, root.to_string_lossy().into_owned()))
}

impl Probe {
    /// The next call of the plugin that the window carries out.
    fn host_call(&mut self) -> HostCall {
        match self.next() {
            MessageKind::Call(call) => call,
            other => panic!("expected a call, got {other:?}"),
        }
    }

    /// The next call of the interfaces the window does the work of, and where to reply.
    fn window_call(&mut self) -> (WindowCall, Sender<WindowReply>) {
        match self.host_call() {
            HostCall::Window { call, reply } => (call, reply),
            other => panic!("expected a window call, got {other:?}"),
        }
    }

    fn command(&mut self, command: &str) {
        self.instance.run_command(command, super::palette());
    }
}

#[test]
fn settings_are_written_and_checked_like_the_settings_page() {
    let Some((mut probe, _)) = v3_probe("test.v3-settings", "project = \"none\"") else {
        return;
    };
    probe.command("v3:settings-set");
    match probe.host_call() {
        HostCall::SetSetting { key, value } => {
            assert_eq!(key, "greeting");
            assert_eq!(value, Some(json!("hey")));
        }
        other => panic!("expected SetSetting, got {other:?}"),
    }
    assert_eq!(probe.notification(), "set: ok");
    // The plugin reads its own write at once.
    assert_eq!(probe.run("setting"), "setting: hey");
    assert_eq!(
        probe.run("v3:settings-set-number"),
        "set failed: settings.set: \"greeting\": it must be a string"
    );
    assert_eq!(
        probe.run("v3:settings-set-status"),
        "set failed: settings.set: \"account-status\" has no value"
    );
    probe.command("v3:settings-reset");
    match probe.host_call() {
        HostCall::SetSetting { key, value } => {
            assert_eq!((key.as_str(), value), ("greeting", None))
        }
        other => panic!("expected SetSetting, got {other:?}"),
    }
    assert_eq!(probe.notification(), "reset: ok");
    assert_eq!(probe.run("setting"), "setting: hello from the manifest");

    probe.command("v3:settings-status");
    match probe.host_call() {
        HostCall::SetSettingStatus { key, status } => {
            assert_eq!(key, "account-status");
            assert_eq!(
                status,
                Some(SettingStatus {
                    text: "Signed in".into(),
                    tone: Tone::Success,
                    busy: false,
                })
            );
        }
        other => panic!("expected SetSettingStatus, got {other:?}"),
    }
    // Not a status: only a warning in the log.
    probe.command("v3:settings-status-wrong");
    assert_eq!(probe.run("hello"), "hello");
    assert!(
        probe
            .log_text()
            .contains("settings.set-status: \"greeting\" isn't a status in flux-plugin.toml")
    );
}

#[test]
fn forms_and_pickers_go_to_the_window_and_answer_with_events() {
    let Some((mut probe, _)) = v3_probe("test.v3-forms", "project = \"none\"") else {
        return;
    };
    probe.command("v3:form");
    let id = match probe.host_call() {
        HostCall::AskForm { id, form } => {
            assert_eq!(form.title, "Open Task");
            assert_eq!(form.fields[1].enabled_by.as_deref(), Some("branch"));
            id
        }
        other => panic!("expected AskForm, got {other:?}"),
    };
    assert_eq!(probe.notification(), format!("form: {id}"));
    probe.instance.send_event(Event::FormAnswered((
        id,
        Some(vec![
            ("branch".into(), "true".into()),
            ("name".into(), " TRACK-9 ".into()),
        ]),
    )));
    assert_eq!(
        probe.notification(),
        format!("form {id}: branch=true name=TRACK-9")
    );
    probe.instance.send_event(Event::FormAnswered((id, None)));
    assert_eq!(probe.notification(), format!("form {id}: cancelled"));

    probe.command("v3:picker");
    let picker = match probe.host_call() {
        HostCall::OpenPicker { id, spec } => {
            assert!(spec.live_query && spec.searchable);
            assert_eq!(spec.anchor, PickerAnchor::StatusItem("clock".into()));
            assert_eq!(spec.items[0].key, "a");
            id
        }
        other => panic!("expected OpenPicker, got {other:?}"),
    };
    probe.command("v3:picker-update");
    match probe.host_call() {
        HostCall::UpdatePicker { id, items, loading } => {
            assert_eq!((id, items[0].key.as_str(), loading), (picker, "b", true));
        }
        other => panic!("expected UpdatePicker, got {other:?}"),
    }
    probe
        .instance
        .send_event(Event::PickerQuery((picker, "tr".into())));
    assert_eq!(probe.notification(), format!("query {picker}: tr"));
    probe.command("v3:picker-close");
    match probe.host_call() {
        HostCall::ClosePicker { id } => assert_eq!(id, picker),
        other => panic!("expected ClosePicker, got {other:?}"),
    }
    probe
        .instance
        .send_event(Event::PickerChosen((picker, Some("b".into()))));
    assert_eq!(
        probe.notification(),
        format!("chosen {picker}: Some(\"b\")")
    );
    // An anchor the manifest doesn't declare: under the title bar, and a warning.
    probe.command("v3:picker-unknown-anchor");
    match probe.host_call() {
        HostCall::OpenPicker { spec, .. } => {
            assert_eq!(spec.anchor, PickerAnchor::Window);
            assert!(!spec.searchable);
        }
        other => panic!("expected OpenPicker, got {other:?}"),
    }
    assert!(
        probe
            .log_text()
            .contains("picker.open: the status bar item \"nope\" isn't in flux-plugin.toml")
    );
}

#[test]
fn branches_and_checkouts_need_write_access() {
    let Some((mut reader, _)) = v3_probe("test.v3-git-read", "project = \"read\"") else {
        return;
    };
    let title = reader.run("v3:new-branch");
    let id = title.strip_prefix("new-branch: ").unwrap().to_string();
    let done = reader.notification();
    assert!(
        done.starts_with(&format!(
            "git-done {id}: Err(\"The plugin may not change the project: no `project = \\\"write\\\"`"
        )),
        "{done}"
    );

    let Some((mut writer, root)) = v3_probe("test.v3-git-write", "project = \"write\"") else {
        return;
    };
    writer.command("v3:new-branch");
    let id = match writer.window_call() {
        (
            WindowCall::GitNewBranch {
                id,
                root: at,
                name,
                start,
            },
            _,
        ) => {
            assert_eq!(at, root);
            assert_eq!(
                (name.as_str(), start.as_deref()),
                ("TRACK-1-fix", Some("main"))
            );
            id
        }
        (other, _) => panic!("expected GitNewBranch, got {other:?}"),
    };
    assert_eq!(writer.notification(), format!("new-branch: {id}"));
    writer.instance.send_event(Event::GitDone((id, Ok(()))));
    assert_eq!(writer.notification(), format!("git-done {id}: Ok(())"));
    writer.command("v3:checkout");
    match writer.window_call() {
        (WindowCall::GitCheckout { branch, .. }, _) => assert_eq!(branch, "main"),
        (other, _) => panic!("expected GitCheckout, got {other:?}"),
    }
    assert!(writer.notification().starts_with("checkout: "));

    writer.command("v3:branches");
    match writer.window_call() {
        (WindowCall::GitBranches(at), reply) => {
            assert_eq!(at, root);
            reply
                .send(WindowReply::Branches(Ok(vec![
                    Branch {
                        name: "main".into(),
                        remote: false,
                        current: true,
                    },
                    Branch {
                        name: "origin/main".into(),
                        remote: true,
                        current: false,
                    },
                ])))
                .unwrap();
        }
        (other, _) => panic!("expected GitBranches, got {other:?}"),
    }
    assert_eq!(writer.notification(), "branches: main,origin/main");
    writer.command("v3:commit-template");
    match writer.window_call() {
        (WindowCall::SetCommitTemplate(template), _) => assert_eq!(
            template,
            Some(CommitTemplate {
                text: "TRACK-1 Fix".into(),
                prefix: Some("TRACK-1".into()),
            })
        ),
        (other, _) => panic!("expected SetCommitTemplate, got {other:?}"),
    }

    let Some((mut stranger, _)) = v3_probe("test.v3-git-none", "project = \"none\"") else {
        return;
    };
    let title = stranger.run("v3:branches");
    assert!(
        title.starts_with("branches failed: The plugin may not read the project"),
        "{title}"
    );
}

#[test]
fn links_claude_and_contexts() {
    let Some((mut probe, _)) = v3_probe("test.v3-links", "project = \"read\"") else {
        return;
    };
    probe.command("v3:links");
    match probe.window_call() {
        (WindowCall::SetLinkPatterns(patterns), reply) => {
            assert_eq!(patterns[0].id, "task");
            assert_eq!(patterns[0].regex, r"\b(?:TRACK)-\d+\b");
            assert!(patterns[0].tooltip);
            reply.send(WindowReply::Done(Ok(()))).unwrap();
        }
        (other, _) => panic!("expected SetLinkPatterns, got {other:?}"),
    }
    assert_eq!(probe.notification(), "links: ok");
    probe
        .instance
        .send_event(Event::LinkHovered((9, "task".into(), "TRACK-5".into())));
    match probe.window_call() {
        (WindowCall::DescribeLink { request, markdown }, _) => {
            assert_eq!((request, markdown.as_deref()), (9, Some("**TRACK-5**")));
        }
        (other, _) => panic!("expected DescribeLink, got {other:?}"),
    }
    probe
        .instance
        .send_event(Event::LinkActivated(("task".into(), "TRACK-5".into())));
    assert_eq!(probe.notification(), "link task: TRACK-5");

    // Claude needs its permission.
    assert_eq!(
        probe.run("v3:claude"),
        "claude failed: The plugin may not open Claude Code sessions: no `claude = true` in the \
         permissions of flux-plugin.toml"
    );

    probe.command("v3:contexts-switch");
    match probe.window_call() {
        (WindowCall::SwitchContext { save, load }, reply) => {
            assert_eq!(
                save,
                Some(ContextRef {
                    key: "TRACK-0".into(),
                    title: "TRACK-0 Old".into(),
                })
            );
            assert_eq!(load.as_deref(), Some("TRACK-1"));
            reply.send(WindowReply::Done(Ok(()))).unwrap();
        }
        (other, _) => panic!("expected SwitchContext, got {other:?}"),
    }
    assert_eq!(probe.notification(), "switch: ok");
    probe.command("v3:contexts-remove");
    match probe.window_call() {
        (WindowCall::RemoveContext(key), _) => assert_eq!(key, "TRACK-0"),
        (other, _) => panic!("expected RemoveContext, got {other:?}"),
    }

    let Some((mut claude, _)) = v3_probe("test.v3-claude", "project = \"none\"\nclaude = true")
    else {
        return;
    };
    claude.command("v3:claude");
    match claude.window_call() {
        (WindowCall::NewClaudeSession(spec), reply) => {
            assert_eq!(
                spec,
                SessionSpec {
                    title: Some("TRACK-1".into()),
                    attachments: vec![Attachment {
                        label: "TRACK-1".into(),
                        markdown: "# Fix".into(),
                    }],
                    prompt: Some("Plan it".into()),
                    send: true,
                    plan_mode: true,
                }
            );
            reply.send(WindowReply::Done(Ok(()))).unwrap();
        }
        (other, _) => panic!("expected NewClaudeSession, got {other:?}"),
    }
    assert_eq!(claude.notification(), "claude: ok");
    // Contexts are the project's: without it, no.
    let title = claude.run("v3:contexts-switch");
    assert!(
        title.starts_with("switch failed: The plugin may not read the project"),
        "{title}"
    );
}

#[test]
fn the_clock_the_time_zone_and_settings() {
    let Some((mut probe, _)) = v3_probe("test.v3-system", "project = \"none\"") else {
        return;
    };
    assert_eq!(
        probe.run("v3:utc-offset"),
        format!("offset: {}", crate::host::local_utc_offset(1_791_961_200))
    );
    assert_eq!(
        probe.run("v3:time-zone"),
        format!("zone: {}", crate::host::local_time_zone())
    );
    probe.command("v3:open-settings");
    match probe.window_call() {
        (WindowCall::OpenSettings, _) => {}
        (other, _) => panic!("expected OpenSettings, got {other:?}"),
    }
}
