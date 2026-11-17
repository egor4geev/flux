//! The plugin's service (API 0.5): two windows' instances and the service exchange messages, the
//! service restarts, a window closes; the plugin's helpers; Claude's tool calls; the numbers of
//! the round trip and of an idle service (`--ignored`).

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use futures::channel::mpsc::{TryRecvError, unbounded};
use serde_json::Map;

use super::{Probe, palette, probe_manifest};
use crate::api::events::Event;
use crate::api::tools::ToolCall;
use crate::log::PluginLog;
use crate::registry::{PluginEntry, PluginSource, load_dir};
use crate::runtime::{HostCall, Instance, InstanceConfig, InstanceRole, MessageKind};

const COMMANDS: &[&str] = &[
    "svc:send",
    "svc:is-service",
    "svc:helper",
    "svc:run-helper",
    "svc:respond",
    "svc:confirm",
];

/// The probe with `service = true` and `extra` (helpers).
fn service_probe(id: &str, extra: &str) -> Option<PluginEntry> {
    let entry = probe_manifest(id, "project = \"none\"", COMMANDS, extra)?;
    let dir = entry.files.dir().unwrap().to_path_buf();
    let file = dir.join("flux-plugin.toml");
    let text = std::fs::read_to_string(&file).unwrap().replacen(
        "wasm = \"probe.wasm\"\n",
        "wasm = \"probe.wasm\"\nservice = true\n",
        1,
    );
    std::fs::write(&file, text).unwrap();
    Some(load_dir(&dir, PluginSource::Installed).unwrap())
}

fn start_as(entry: &PluginEntry, role: InstanceRole) -> Probe {
    let (messages, receiver) = unbounded();
    let log = PluginLog::memory();
    let instance = Instance::start(InstanceConfig {
        role,
        entry: Arc::new(entry.clone()),
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
    probe.expect_started();
    probe
}

impl Probe {
    fn with_argument(&self, command: &str, argument: &str) {
        self.instance.run_command(
            command,
            crate::api::types::CommandContext {
                argument: Some(argument.into()),
                ..palette()
            },
        );
    }

    /// Sends `message` to the service.
    fn to_service(&self, message: &str) {
        self.with_argument("svc:send", message);
    }
}

#[test]
fn two_windows_and_the_service_exchange_messages() {
    let Some(entry) = service_probe("test.svc-messages", "") else {
        return;
    };
    let mut one = start_as(&entry, InstanceRole::Window(101));
    let mut two = start_as(&entry, InstanceRole::Window(102));
    // No service yet: a message has nobody to take it.
    one.to_service("ping 0");
    assert_eq!(
        one.notification(),
        "send failed: The plugin's service isn't running"
    );
    let mut service = start_as(&entry, InstanceRole::Service);
    assert_eq!(one.notification(), "service-started");
    assert_eq!(two.notification(), "service-started");
    // A window asks, the service answers it alone.
    one.to_service("ping 1");
    assert_eq!(one.notification(), "from service: pong 1");
    // A counter shared by all windows: the service tells everyone.
    two.to_service("count");
    one.to_service("count");
    for window in [&mut one, &mut two] {
        assert_eq!(window.notification(), "from service: count 1");
        assert_eq!(window.notification(), "from service: count 2");
    }
    // In order.
    for n in 0..50 {
        one.to_service(&format!("ping {n}"));
    }
    for n in 0..50 {
        assert_eq!(one.notification(), format!("from service: pong {n}"));
    }
    two.quiet(Duration::from_millis(100));
    // The service knows the windows; its notifications have ids of their own.
    one.to_service("windows");
    match service.next() {
        MessageKind::Call(HostCall::Notify { id, notification }) => {
            assert_eq!(notification.title, "windows: [101, 102]");
            assert!(id > crate::host::SERVICE_IDS);
        }
        other => panic!("expected a notification, got {other:?}"),
    }
    // Roles.
    one.with_argument("svc:is-service", "");
    assert_eq!(one.notification(), "is-service: false");
    service.with_argument("svc:is-service", "");
    assert_eq!(service.notification(), "is-service: true");
    // A window's interfaces in the service: empty answers, a question dismissed, no stop; the
    // window never hears of them.
    one.to_service("editor");
    let title = service.notification();
    assert!(
        title.starts_with("editor: none=true failed=true ask="),
        "{title}"
    );
    let ask = title.rsplit('=').next().unwrap().to_string();
    assert_eq!(service.notification(), format!("answered {ask}: None"));
    let log = service.log_text();
    assert!(
        log.contains("dialogs.ask: not in the plugin's service — it has no window"),
        "{log}"
    );
    // A window closes: the service hears it, and a message to it fails.
    drop(two);
    assert_eq!(service.notification(), "window-closed 102");
    one.to_service("to 102 hi");
    assert_eq!(service.notification(), "to failed: The window 102 is gone");
    one.to_service("to 101 hi");
    assert_eq!(one.notification(), "from service: hi");
    // A new window: the service hears it.
    let mut three = start_as(&entry, InstanceRole::Window(103));
    assert_eq!(service.notification(), "window-opened 103");
    three.to_service("ping 3");
    assert_eq!(three.notification(), "from service: pong 3");
    one.quiet(Duration::from_millis(100));
}

#[test]
fn a_service_that_stopped_and_started_again_tells_the_windows() {
    let Some(entry) = service_probe("test.svc-restart", "") else {
        return;
    };
    let mut window = start_as(&entry, InstanceRole::Window(201));
    let mut service = start_as(&entry, InstanceRole::Service);
    assert_eq!(window.notification(), "service-started");
    window.to_service("count");
    assert_eq!(window.notification(), "from service: count 1");
    // The service panics: it is gone, and the window's messages fail.
    service.instance.run_command("panic", palette());
    let (error, _) = service.stopped();
    assert_eq!(error, "panicked: boom 42");
    window.to_service("count");
    assert_eq!(
        window.notification(),
        "send failed: The plugin's service isn't running"
    );
    // Started again: the window hears it, and the service starts from nothing.
    let _service = start_as(&entry, InstanceRole::Service);
    assert_eq!(window.notification(), "service-started");
    window.to_service("count");
    assert_eq!(window.notification(), "from service: count 1");
}

#[test]
fn a_plugin_without_a_service_cant_send() {
    let Some(entry) = probe_manifest("test.svc-none", "project = \"none\"", COMMANDS, "") else {
        return;
    };
    let mut window = start_as(&entry, InstanceRole::Window(301));
    window.to_service("ping");
    assert_eq!(
        window.notification(),
        "send failed: service.send: flux-plugin.toml doesn't say service = true"
    );
}

#[test]
fn claudes_tool_calls_are_followed() {
    let Some(entry) = service_probe("test.svc-tools", "") else {
        return;
    };
    let mut window = start_as(&entry, InstanceRole::Window(401));
    let call = |id| {
        Event::ToolCalled(ToolCall {
            id,
            tool: "read_chat".into(),
            arguments: "{}".into(),
        })
    };
    window.instance.send_event(call(5));
    assert_eq!(window.notification(), "tool-called 5 read_chat");
    window.with_argument("svc:confirm", "5");
    match window.next() {
        MessageKind::Call(HostCall::ToolConfirm { call: 5, card }) => {
            assert_eq!(card.confirm, "Send")
        }
        other => panic!("expected the card, got {other:?}"),
    }
    window.with_argument("svc:respond", "5");
    match window.next() {
        MessageKind::Call(HostCall::ToolRespond { call: 5, output }) => {
            assert_eq!(output.text, "done");
            assert_eq!(output.summary.as_deref(), Some("Done"));
        }
        other => panic!("expected the answer, got {other:?}"),
    }
    // Answered: the call is gone.
    window.with_argument("svc:confirm", "5");
    assert_eq!(
        window.notification(),
        "confirm failed: tools.confirm: the call 5 is gone"
    );
    // Cancelled: no answer, no card.
    window.instance.send_event(call(6));
    assert_eq!(window.notification(), "tool-called 6 read_chat");
    window.instance.send_event(Event::ToolCancelled(6));
    assert_eq!(window.notification(), "tool-cancelled 6");
    window.with_argument("svc:respond", "6");
    window.with_argument("svc:confirm", "6");
    assert_eq!(
        window.notification(),
        "confirm failed: tools.confirm: the call 6 is gone"
    );
    assert!(
        window
            .log_text()
            .contains("tools.respond: the call 6 is gone")
    );
}

// --- Helpers ---

/// A server of the test's own: every request gets `body` (after `pause` in the middle of it);
/// the count of requests.
fn file_server(body: Vec<u8>, pause: Duration) -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let count = Arc::new(AtomicUsize::new(0));
    let served = count.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            served.fetch_add(1, Ordering::SeqCst);
            let body = body.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                }
                let mut stream = stream;
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let half = body.len() / 2;
                let _ = stream.write_all(&body[..half]);
                let _ = stream.flush();
                std::thread::sleep(pause);
                let _ = stream.write_all(&body[half..]);
            });
        }
    });
    (port, count)
}

#[cfg(unix)]
fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

/// The manifest's lines of a helper `name` at `url` with `sha256`, for this platform.
fn helper_lines(name: &str, url: &str, sha256: &str) -> String {
    let platform = crate::manifest::helper_platform().unwrap();
    format!(
        "[[helpers]]\nname = \"{name}\"\n[helpers.platforms.{platform}]\nurl = \"{url}\"\nsha256 \
         = \"{sha256}\"\n"
    )
}

#[cfg(unix)]
#[test]
fn a_helper_is_downloaded_once_checked_and_run() {
    let script = b"#!/bin/sh\necho \"helper $1\"\n".to_vec();
    let packed = gzip(&script);
    let sha256 = crate::catalog::sha256_hex(&packed);
    let (port, requests) = file_server(packed, Duration::ZERO);
    let url = "https://example.invalid/probe-helper.gz";
    crate::helpers::test_redirects().lock().unwrap().insert(
        url.into(),
        format!("http://127.0.0.1:{port}/probe-helper.gz"),
    );
    let Some(entry) = service_probe(
        "test.svc-helper",
        &helper_lines("probe-helper", url, &sha256),
    ) else {
        return;
    };
    // Two windows and the service ask at once: one download.
    let mut one = start_as(&entry, InstanceRole::Window(501));
    let mut two = start_as(&entry, InstanceRole::Window(502));
    let mut service = start_as(&entry, InstanceRole::Service);
    assert_eq!(one.notification(), "service-started");
    assert_eq!(two.notification(), "service-started");
    for probe in [&one, &two, &service] {
        probe.with_argument("svc:helper", "probe-helper");
    }
    let expected = crate::paths::helpers_dir()
        .join("test.svc-helper")
        .join(&sha256)
        .join("probe-helper");
    for probe in [&mut one, &mut two, &mut service] {
        assert_eq!(
            probe.notification(),
            format!("helper: {}", expected.display())
        );
    }
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    // It runs without the `processes` permission, and isn't downloaded again.
    one.with_argument("svc:run-helper", "probe-helper");
    assert_eq!(one.notification(), "ran: helper hi");
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    // An unknown one fails.
    one.with_argument("svc:helper", "nope");
    assert_eq!(
        one.notification(),
        "helper failed: There's no helper \"nope\" in flux-plugin.toml ([[helpers]])"
    );
    // A new version replaces the old one.
    let newer = gzip(b"#!/bin/sh\necho \"newer $1\"\n");
    let newer_sha = crate::catalog::sha256_hex(&newer);
    let (port, _) = file_server(newer, Duration::ZERO);
    let newer_url = "https://example.invalid/probe-helper-2.gz";
    crate::helpers::test_redirects().lock().unwrap().insert(
        newer_url.into(),
        format!("http://127.0.0.1:{port}/probe-helper-2.gz"),
    );
    let mut updated = entry.clone();
    updated.manifest.helpers = crate::manifest::Manifest::parse(&format!(
        "id = \"test.svc-helper\"\nname = \"P\"\nversion = \"0.2.0\"\napi = \"0.5\"\n{}",
        helper_lines("probe-helper", newer_url, &newer_sha)
    ))
    .unwrap()
    .helpers;
    let mut three = start_as(&updated, InstanceRole::Window(503));
    three.with_argument("svc:run-helper", "probe-helper");
    assert_eq!(three.notification(), "ran: newer hi");
    assert!(!expected.exists(), "the old version stays");
}

#[test]
fn a_helper_with_a_wrong_checksum_isnt_kept_and_a_long_download_is_shown() {
    let (port, _) = file_server(vec![7; 300_000], Duration::from_millis(1300));
    let url = "https://example.invalid/slow-helper";
    crate::helpers::test_redirects()
        .lock()
        .unwrap()
        .insert(url.into(), format!("http://127.0.0.1:{port}/slow-helper"));
    let Some(entry) = service_probe(
        "test.svc-helper-bad",
        &helper_lines("slow-helper", url, &"0".repeat(64)),
    ) else {
        return;
    };
    let mut window = start_as(&entry, InstanceRole::Window(601));
    window.with_argument("svc:helper", "slow-helper");
    let mut shown = 0;
    let title = loop {
        match window.next() {
            MessageKind::Call(HostCall::HelperProgress { helper, progress }) => {
                assert_eq!(helper, "slow-helper");
                match progress {
                    Some((done, total)) => {
                        assert_eq!(total, 300_000);
                        assert!(done <= total);
                        shown += 1;
                    }
                    None => assert!(shown > 0, "ended before it began"),
                }
            }
            MessageKind::Call(HostCall::Notify { notification, .. }) => break notification.title,
            other => panic!("expected the progress, got {other:?}"),
        }
    };
    assert!(shown > 0, "a download over a second isn't shown");
    assert!(
        title.starts_with("helper failed: The helper \"slow-helper\" from https://example.invalid/slow-helper doesn't match"),
        "{title}"
    );
    assert!(
        !crate::paths::helpers_dir()
            .join("test.svc-helper-bad")
            .join("0".repeat(64))
            .join("slow-helper")
            .exists()
    );
}

// --- Numbers ---

/// The round trip window → service → window over 1000 messages, and the CPU of an idle service:
/// `cargo test -p flux-plugin service_numbers -- --ignored --nocapture`.
#[test]
#[ignore]
fn service_numbers() {
    let Some(entry) = service_probe("test.svc-numbers", "") else {
        return;
    };
    let mut window = start_as(&entry, InstanceRole::Window(701));
    let _service = start_as(&entry, InstanceRole::Service);
    assert_eq!(window.notification(), "service-started");
    let mut times = Vec::new();
    for n in 0..1000 {
        let started = Instant::now();
        window.to_service(&format!("ping {n}"));
        // Polls without sleeping: `next` sleeps 5 ms between looks.
        let title = loop {
            match window.messages.try_recv() {
                Ok(message) => match message.kind {
                    MessageKind::Call(HostCall::Notify { notification, .. }) => {
                        break notification.title;
                    }
                    _ => continue,
                },
                Err(TryRecvError::Empty) => std::hint::spin_loop(),
                Err(TryRecvError::Closed) => panic!("closed"),
            }
        };
        times.push(started.elapsed());
        assert_eq!(title, format!("from service: pong {n}"));
    }
    times.sort();
    let at = |q: f64| times[((times.len() as f64 * q) as usize).min(times.len() - 1)];
    eprintln!(
        "round trip window → service → window over 1000 (the command into the window, the \
         message, the answer, the window's notification): p50 {:?}, p99 {:?}, max {:?}",
        at(0.5),
        at(0.99),
        times.last().unwrap()
    );
    // The idle CPU through `getrusage` — Unix only (the numbers are taken on the Mac).
    #[cfg(unix)]
    {
        let cpu = || {
            let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
            // SAFETY: getrusage fills the struct it is given.
            unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
            // SAFETY: filled above.
            let usage = unsafe { usage.assume_init() };
            let time = |t: libc::timeval| Duration::new(t.tv_sec as u64, t.tv_usec as u32 * 1000);
            time(usage.ru_utime) + time(usage.ru_stime)
        };
        std::thread::sleep(Duration::from_millis(500));
        let before = cpu();
        let idle = Duration::from_secs(5);
        std::thread::sleep(idle);
        let used = cpu() - before;
        eprintln!(
            "idle: a window's instance and the service with nothing to do (and the engine's epoch \
             ticker, shared by every plugin): {:?} of CPU in {idle:?} = {:.3} %",
            used,
            used.as_secs_f64() / idle.as_secs_f64() * 100.
        );
    }
}
