//! The runtime against a real component: the probe plugin (`tests/fixtures/probe`), built for
//! `wasm32-wasip2` by the first test that needs it (a test is skipped, with a note, when it can't
//! be built). Everything the plugins write goes into a temporary folder of this run.

/// Requests and the plugin's server (part 8.2, agent A).
mod net;
/// Programs, timers, secrets, folders, the time limit (part 8.2, agent F).
mod process;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use futures::channel::mpsc::{TryRecvError, UnboundedReceiver, unbounded};
use serde_json::{Map, Value, json};

use crate::api::events::Event;
use crate::api::types::EditorInfo;
use crate::log::PluginLog;
use crate::registry::{PluginEntry, PluginSource, load_dir};
use crate::runtime::{EditorCall, EditorReply, HostCall, Instance, InstanceConfig, MessageKind};

/// This run's folder: the plugins' home (data, cache, logs), the plugins, the projects.
pub(crate) fn scratch() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("flux-plugin-tests-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::paths::override_home(dir.join("home"));
        dir
    })
}

/// A new empty folder in the scratch folder.
pub(crate) fn temp_dir(name: &str) -> PathBuf {
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    let n = COUNT.fetch_add(1, Ordering::Relaxed);
    let dir = scratch().join(format!("{name}-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The probe plugin's component.
fn probe_wasm() -> Option<&'static [u8]> {
    static WASM: OnceLock<Option<Vec<u8>>> = OnceLock::new();
    WASM.get_or_init(|| {
        let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let fixture = crate_dir.join("tests/fixtures/probe");
        let target = crate_dir.join("../../target/plugin-fixtures");
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let mut command = Command::new(cargo);
        command
            .args([
                "build",
                "--release",
                "--quiet",
                "--target",
                crate::dev::TARGET,
            ])
            .arg("--target-dir")
            .arg(&target)
            .current_dir(&fixture);
        crate::dev::clean_env(&mut command);
        let output = command.output().ok()?;
        if !output.status.success() {
            eprintln!(
                "skipped: the probe plugin didn't build (is the wasm32-wasip2 target \
                 installed?):\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return None;
        }
        std::fs::read(target.join("wasm32-wasip2/release/flux_plugin_probe.wasm")).ok()
    })
    .as_deref()
}

/// A plugin folder with the probe component, a manifest with `project` access and the probe's
/// commands, and a Russian table.
fn probe(id: &str, project: &str) -> Option<PluginEntry> {
    probe_with(id, &format!("project = \"{project}\""), &[])
}

/// The probe with `permissions` (the lines of `[permissions]`) and more commands (`net:…`,
/// `proc:…`).
fn probe_with(id: &str, permissions: &str, more_commands: &[&str]) -> Option<PluginEntry> {
    let wasm = probe_wasm()?;
    let dir = temp_dir(id);
    let commands = [
        "context",
        "hello",
        "echo",
        "panic",
        "spin",
        "oom",
        "search",
        "store",
        "load",
        "read-project",
        "write-data",
        "setting",
        "translate",
    ]
    .iter()
    .chain(more_commands)
    .map(|command| format!("[[commands]]\nid = \"{command}\"\ntitle = \"{command}\"\n"))
    .collect::<Vec<_>>()
    .join("\n");
    let manifest = format!(
        "id = \"{id}\"\nname = \"Probe\"\nversion = \"0.1.0\"\napi = \"0.2\"\nwasm = \
         \"probe.wasm\"\n\n[permissions]\n{permissions}\n\n{commands}\n\
         [[settings]]\nkey = \"greeting\"\ntitle = \"Greeting\"\ntype = \"string\"\ndefault = \
         \"hello from the manifest\"\n"
    );
    std::fs::write(dir.join("flux-plugin.toml"), manifest).unwrap();
    std::fs::write(dir.join("probe.wasm"), wasm).unwrap();
    std::fs::create_dir_all(dir.join("locales")).unwrap();
    std::fs::write(dir.join("locales/ru.toml"), "\"Hello\" = \"Привет\"\n").unwrap();
    Some(load_dir(&dir, PluginSource::Installed).unwrap())
}

/// The context of a command run from the palette, with no document.
fn palette() -> crate::api::types::CommandContext {
    crate::api::types::CommandContext {
        source: crate::api::types::CommandSource::Palette,
        editor: None,
        selections: Vec::new(),
        paths: Vec::new(),
    }
}

/// A running probe and its messages.
struct Probe {
    instance: Instance,
    messages: UnboundedReceiver<crate::runtime::PluginMessage>,
    log: PluginLog,
}

fn start(entry: PluginEntry, root: Option<PathBuf>, settings: Map<String, Value>) -> Probe {
    start_in(entry, root, settings, "en")
}

fn start_in(
    entry: PluginEntry,
    root: Option<PathBuf>,
    settings: Map<String, Value>,
    language: &str,
) -> Probe {
    let (messages, receiver) = unbounded();
    let log = PluginLog::memory();
    let instance = Instance::start(InstanceConfig {
        entry: std::sync::Arc::new(entry),
        project_root: root,
        language: language.into(),
        settings,
        messages,
        log: log.clone(),
    });
    let mut probe = Probe {
        instance,
        messages: receiver,
        log,
    };
    probe.expect_started();
    probe
}

const WAIT: Duration = Duration::from_secs(30);

impl Probe {
    /// The next message other than `Logged`.
    fn next(&mut self) -> MessageKind {
        let deadline = Instant::now() + WAIT;
        loop {
            match self.messages.try_recv() {
                Ok(message) => match message.kind {
                    MessageKind::Logged => continue,
                    kind => return kind,
                },
                Err(TryRecvError::Closed) => panic!("the instance's channel is closed"),
                Err(TryRecvError::Empty) if Instant::now() > deadline => {
                    panic!("no message in {WAIT:?}")
                }
                Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
    }

    fn expect_started(&mut self) {
        match self.next() {
            MessageKind::Started => {}
            other => panic!("expected Started, got {other:?}"),
        }
    }

    /// The title of the next notification.
    fn notification(&mut self) -> String {
        match self.next() {
            MessageKind::Call(HostCall::Notify { notification, .. }) => notification.title,
            other => panic!("expected a notification, got {other:?}"),
        }
    }

    fn run(&mut self, command: &str) -> String {
        self.instance.run_command(command, palette());
        self.notification()
    }

    fn stopped(&mut self) -> (String, String) {
        match self.next() {
            MessageKind::Stopped { error, details } => (error, details),
            other => panic!("expected Stopped, got {other:?}"),
        }
    }

    fn log_text(&self) -> String {
        self.log
            .lines()
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[test]
fn starts_runs_commands_and_logs_its_output() {
    let Some(entry) = probe("test.hello", "none") else {
        return;
    };
    let mut probe = start(entry, None, Map::new());
    assert_eq!(probe.run("hello"), "hello");
    let log = probe.log_text();
    assert!(log.contains("probe: activated"), "{log}");
    assert!(log.contains("probe: on stderr"), "{log}");
}

#[test]
fn asks_the_window_about_documents() {
    let Some(entry) = probe("test.echo", "none") else {
        return;
    };
    let mut probe = start(entry, None, Map::new());
    probe.instance.run_command("echo", palette());
    let info = EditorInfo {
        id: 7,
        path: Some("/tmp/main.rs".into()),
        language: "Rust".into(),
        modified: false,
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
            call: EditorCall::Text(7),
            reply,
        }) => reply
            .send(EditorReply::Text(Some("fn main() {}".into())))
            .unwrap(),
        other => panic!("expected the text call, got {other:?}"),
    }
    assert_eq!(probe.notification(), "text: fn main() {}");
}

#[test]
fn a_panic_stops_the_plugin() {
    let Some(entry) = probe("test.panic", "none") else {
        return;
    };
    let mut probe = start(entry, None, Map::new());
    probe.instance.run_command("panic", palette());
    let (error, details) = probe.stopped();
    assert_eq!(error, "panicked: boom 42");
    assert!(details.contains("boom 42"), "{details}");
    assert!(details.contains("wasm backtrace"), "{details}");
}

#[test]
fn an_endless_call_is_stopped_by_the_time_limit() {
    let Some(entry) = probe("test.spin", "none") else {
        return;
    };
    crate::runtime::test_limits()
        .lock()
        .unwrap()
        .insert("test.spin".into(), Duration::from_millis(300));
    let mut probe = start(entry, None, Map::new());
    let started = Instant::now();
    probe.instance.run_command("spin", palette());
    let (error, _) = probe.stopped();
    assert_eq!(error, "took longer than 300 ms");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn running_out_of_memory_stops_the_plugin() {
    let Some(entry) = probe("test.oom", "none") else {
        return;
    };
    let mut probe = start(entry, None, Map::new());
    probe.instance.run_command("oom", palette());
    let (error, _) = probe.stopped();
    assert_eq!(error, "ran out of memory (512 MB)");
}

#[test]
fn searches_the_project_it_may_read() {
    let project = temp_dir("search-project");
    std::fs::write(project.join("a.rs"), "fn main() {}\n// TODO: x\n").unwrap();
    std::fs::create_dir_all(project.join("sub")).unwrap();
    std::fs::write(project.join("sub/b.txt"), "TODO first\n").unwrap();
    let Some(entry) = probe("test.search", "read") else {
        return;
    };
    let mut reader = start(entry, Some(project.clone()), Map::new());
    assert_eq!(
        reader.run("search"),
        "found: a.rs:1:[(3, 7)] sub/b.txt:0:[(0, 4)]"
    );
    let Some(entry) = probe("test.search-none", "none") else {
        return;
    };
    let mut stranger = start(entry, Some(project), Map::new());
    let title = stranger.run("search");
    assert!(
        title.starts_with("search failed: The plugin may not read"),
        "{title}"
    );
}

#[test]
fn the_project_is_closed_without_the_permission() {
    let project = temp_dir("sandbox-project");
    std::fs::write(project.join("notes.txt"), "secret\n").unwrap();
    let (Some(closed), Some(open)) = (probe("test.closed", "none"), probe("test.open", "read"))
    else {
        return;
    };
    let mut without = start(closed, Some(project.clone()), Map::new());
    let title = without.run("read-project");
    assert!(title.starts_with("can't read:"), "{title}");
    assert_eq!(without.run("write-data"), "wrote data");
    let mut with = start(open, Some(project), Map::new());
    assert_eq!(with.run("read-project"), "read: secret");
}

#[test]
fn storage_outlives_the_instance() {
    let Some(entry) = probe("test.storage", "none") else {
        return;
    };
    let mut first = start(entry.clone(), None, Map::new());
    first.instance.run_command("store", palette());
    assert_eq!(first.run("load"), "stored: hi there");
    drop(first);
    let mut second = start(entry, None, Map::new());
    assert_eq!(second.run("load"), "stored: hi there");
}

#[test]
fn settings_and_translations() {
    let Some(entry) = probe("test.settings", "none") else {
        return;
    };
    let mut probe = start_in(entry, None, Map::new(), "ru");
    assert_eq!(probe.run("setting"), "setting: hello from the manifest");
    assert_eq!(probe.run("translate"), "Привет");
    let settings = Map::from_iter([("greeting".to_string(), json!("hey"))]);
    probe.instance.set_settings(settings);
    probe.instance.send_event(Event::SettingsChanged);
    assert_eq!(probe.notification(), "settings changed");
    assert_eq!(probe.run("setting"), "setting: hey");
}

#[test]
fn a_new_project_root_restarts_a_plugin_with_access() {
    let first = temp_dir("root-a");
    std::fs::write(first.join("notes.txt"), "first\n").unwrap();
    let second = temp_dir("root-b");
    std::fs::write(second.join("notes.txt"), "second\n").unwrap();
    let Some(entry) = probe("test.root", "read") else {
        return;
    };
    let mut probe = start(entry, Some(first), Map::new());
    assert_eq!(probe.run("read-project"), "read: first");
    probe.instance.set_project_root(Some(second));
    probe.expect_started();
    assert_eq!(probe.run("read-project"), "read: second");
    assert!(probe.log_text().contains("probe: deactivated"));
}

#[test]
fn the_second_load_comes_from_the_cache() {
    let Some(entry) = probe("test.cache", "none") else {
        return;
    };
    // The first instance of the run may compile the component or find it (another test did).
    let first = start(entry.clone(), None, Map::new());
    let second = start(entry, None, Map::new());
    let first_log = first.log_text();
    let second_log = second.log_text();
    eprintln!("first load: {first_log}\nsecond load: {second_log}");
    assert!(second_log.contains("Loaded from the cache"), "{second_log}");
}

#[test]
fn a_plugin_without_code_starts_and_ignores_calls() {
    let dir = temp_dir("declarative");
    std::fs::write(
        dir.join("flux-plugin.toml"),
        "id = \"test.theme\"\nname = \"Theme\"\nversion = \"1.0.0\"\napi = \"0.2\"\n",
    )
    .unwrap();
    let entry = load_dir(&dir, PluginSource::Installed).unwrap();
    let mut probe = start(entry, None, Map::new());
    probe.instance.run_command("anything", palette());
    probe.instance.send_event(Event::SettingsChanged);
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        matches!(probe.messages.try_recv(), Err(TryRecvError::Empty)),
        "no messages expected"
    );
}

#[test]
fn a_stopped_instance_says_nothing_more() {
    let Some(entry) = probe("test.quiet", "none") else {
        return;
    };
    let mut probe = start(entry, None, Map::new());
    probe.instance.run_command("hello", palette());
    assert_eq!(probe.notification(), "hello");
    let Probe {
        instance,
        mut messages,
        log,
    } = probe;
    instance.run_command("hello", palette());
    instance.run_command("panic", palette());
    drop(instance);
    // The queued calls run (the panic too), and `deactivate` doesn't: the plugin trapped.
    std::thread::sleep(Duration::from_millis(300));
    while let Ok(message) = messages.try_recv() {
        assert!(
            matches!(message.kind, MessageKind::Logged),
            "unexpected {:?}",
            message.kind
        );
    }
    assert!(!log.lines().is_empty());
}
