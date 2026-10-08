//! The client against a fake server (`tests/fake_server.py`, needs `python3`): lifecycle, requests,
//! cancellation, notifications both ways, server requests, crashes.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;
use std::thread;
use std::time::{Duration, Instant};

use flux_lsp::lsp_types::notification::{DidOpenTextDocument, Notification};
use flux_lsp::lsp_types::request::{Completion, HoverRequest, Request};
use flux_lsp::lsp_types::{
    CompletionParams, DidOpenTextDocumentParams, HoverContents, HoverParams, MarkupContent,
    MessageType, Position, TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams,
    Uri,
};
use flux_lsp::{LanguageServer, RequestError, ServerConfig, ServerEvent};
use futures::channel::mpsc::{TryRecvError, UnboundedReceiver};
use serde_json::Value;

const TIMEOUT: Duration = Duration::from_secs(10);

struct Fake {
    dir: tempfile::TempDir,
    log: PathBuf,
}

impl Fake {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log.jsonl");
        Self { dir, log }
    }

    fn config(&self, extra: &[&str]) -> ServerConfig {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake_server.py");
        let mut args = vec![
            script.to_string_lossy().into_owned(),
            self.log.to_string_lossy().into_owned(),
        ];
        args.extend(extra.iter().map(|s| s.to_string()));
        ServerConfig {
            name: "fake".into(),
            command: "python3".into(),
            args,
            extensions: vec!["rs".into()],
            file_names: vec![],
            initialization_options: None,
            settings: None,
            install: None,
        }
    }

    fn start(&self, extra: &[&str]) -> (LanguageServer, UnboundedReceiver<ServerEvent>) {
        LanguageServer::start(&self.config(extra), self.dir.path()).unwrap()
    }

    /// Messages the fake server received, in order.
    fn received(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn methods(&self) -> Vec<String> {
        self.received()
            .iter()
            .filter_map(|m| m["method"].as_str().map(str::to_owned))
            .collect()
    }

    fn pid(&self) -> u32 {
        self.received()[0]["pid"].as_u64().unwrap() as u32
    }

    /// Waits until the fake server has received a message matching `f`.
    fn wait_received(&self, f: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(message) = self.received().into_iter().find(&f) {
                return message;
            }
            assert!(
                Instant::now() < deadline,
                "not received: {:?}",
                self.received()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}

fn next_event(events: &mut UnboundedReceiver<ServerEvent>) -> ServerEvent {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match events.try_recv() {
            Ok(event) => return event,
            Err(TryRecvError::Closed) => panic!("the event stream ended"),
            Err(TryRecvError::Empty) => {
                assert!(Instant::now() < deadline, "no event");
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

/// Waits for the first event matching `f`, skipping the others.
fn wait_event(
    events: &mut UnboundedReceiver<ServerEvent>,
    f: impl Fn(&ServerEvent) -> bool,
) -> ServerEvent {
    loop {
        let event = next_event(events);
        if f(&event) {
            return event;
        }
    }
}

fn block_on<T: Send + 'static>(future: impl Future<Output = T> + Send + 'static) -> T {
    let (sender, receiver) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(futures::executor::block_on(future));
    });
    receiver
        .recv_timeout(TIMEOUT)
        .expect("the future didn't resolve")
}

fn uri(fake: &Fake) -> Uri {
    flux_lsp::position::uri_from_path(&fake.dir.path().join("main.rs"))
}

fn hover_params(uri: Uri, line: u32) -> HoverParams {
    HoverParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position::new(line, 0),
        },
        work_done_progress_params: Default::default(),
    }
}

fn process_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn wait_dead(pid: u32) {
    let deadline = Instant::now() + TIMEOUT;
    while process_alive(pid) {
        assert!(Instant::now() < deadline, "process {pid} is still alive");
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn lifecycle_requests_and_notifications() {
    let fake = Fake::new();
    let (server, mut events) = fake.start(&[]);
    assert_eq!(server.name(), "fake");

    // Before initialization: requests are refused, notifications are queued.
    let early = server.request::<HoverRequest>(hover_params(uri(&fake), 0));
    assert_eq!(block_on(early), Err(RequestError::NotReady));
    server.notify::<DidOpenTextDocument>(DidOpenTextDocumentParams {
        text_document: TextDocumentItem::new(uri(&fake), "rust".into(), 1, "fn main() {}".into()),
    });
    assert!(server.capabilities().is_none());

    assert!(matches!(next_event(&mut events), ServerEvent::Initialized));
    let capabilities = server.capabilities().unwrap();
    assert!(capabilities.hover_provider.is_some());

    // After `initialized` the server reports progress and a message; the queued didOpen went
    // after `initialized` too, and the server answered it with diagnostics.
    // (`window/logMessage` in between is not an event.)
    let received: Vec<ServerEvent> = (0..5).map(|_| next_event(&mut events)).collect();
    assert!(matches!(
        &received[0],
        ServerEvent::Progress { title, percentage: Some(0), done: false, .. }
            if title.as_deref() == Some("Indexing")
    ));
    assert!(matches!(
        &received[1],
        ServerEvent::Progress {
            title: None,
            percentage: Some(50),
            done: false,
            ..
        }
    ));
    assert!(matches!(
        &received[2],
        ServerEvent::Progress { done: true, .. }
    ));
    assert!(matches!(
        &received[3],
        ServerEvent::Message { kind: MessageType::ERROR, text } if text == "hello"
    ));
    let ServerEvent::Diagnostics(diagnostics) = &received[4] else {
        panic!("{received:?}");
    };
    assert_eq!(diagnostics.uri, uri(&fake));
    assert_eq!(diagnostics.version, Some(1));
    assert_eq!(diagnostics.diagnostics[0].message, "bad");
    let methods = fake.methods();
    let position = |m: &str| methods.iter().position(|x| x == m).unwrap();
    assert!(position("initialize") < position("initialized"));
    assert!(position("initialized") < position(DidOpenTextDocument::METHOD));
    let initialize = &fake.received()[1];
    assert_eq!(initialize["method"], "initialize");
    assert_eq!(
        initialize["params"]["capabilities"]["general"]["positionEncodings"],
        serde_json::json!(["utf-16"])
    );
    assert_eq!(
        initialize["params"]["rootUri"].as_str(),
        Some(flux_lsp::position::uri_from_path(fake.dir.path()).as_str())
    );
    assert!(initialize["params"].get("initializationOptions").is_none());

    // A response.
    let hover = block_on(server.request::<HoverRequest>(hover_params(uri(&fake), 0)))
        .unwrap()
        .unwrap();
    assert_eq!(
        hover.contents,
        HoverContents::Markup(MarkupContent {
            kind: flux_lsp::lsp_types::MarkupKind::Markdown,
            value: "**doc**".into(),
        })
    );

    // An error response.
    let completion = server.request::<Completion>(CompletionParams {
        text_document_position: hover_params(uri(&fake), 0).text_document_position_params,
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
        context: None,
    });
    let error = block_on(completion).unwrap_err();
    assert!(error.is_outdated(), "{error:?}");

    // Server requests were answered.
    let response = |id: Value| fake.wait_received(|m| m["id"] == id && m.get("method").is_none());
    assert_eq!(
        response("conf-1".into())["result"],
        serde_json::json!([null, null])
    );
    assert_eq!(response(100.into())["result"], Value::Null);
    assert_eq!(response(101.into())["error"]["code"], -32601);
    assert_eq!(response(102.into())["result"]["applied"], false);

    // Cancellation: the request was sent, then the dropped future cancels it.
    let pending = server.request::<HoverRequest>(hover_params(uri(&fake), 99));
    let sent = fake.wait_received(|m| m["params"]["position"]["line"] == 99);
    drop(pending);
    let cancel = fake.wait_received(|m| m["method"] == "$/cancelRequest");
    assert_eq!(cancel["params"]["id"], sent["id"]);

    // Graceful shutdown: `shutdown`, then `exit`.
    let pid = fake.pid();
    block_on(server.shutdown());
    let methods = fake.methods();
    assert_eq!(&methods[methods.len() - 2..], ["shutdown", "exit"]);
    assert!(matches!(
        wait_event(&mut events, |e| matches!(e, ServerEvent::Exited { .. })),
        ServerEvent::Exited { ref reason } if reason.contains("0")
    ));
    wait_dead(pid);
    assert_eq!(
        block_on(server.request::<HoverRequest>(hover_params(uri(&fake), 0))),
        Err(RequestError::Exited)
    );
}

/// A custom request: the fake server exits on it with a message on stderr.
enum Crash {}

impl Request for Crash {
    type Params = ();
    type Result = ();
    const METHOD: &'static str = "flux/crash";
}

#[test]
fn a_crash_fails_pending_requests_and_reports_stderr() {
    let fake = Fake::new();
    let (server, mut events) = fake.start(&[]);
    assert!(matches!(next_event(&mut events), ServerEvent::Initialized));
    let hanging = server.request::<HoverRequest>(hover_params(uri(&fake), 99));
    fake.wait_received(|m| m["params"]["position"]["line"] == 99);
    assert_eq!(
        block_on(server.request::<Crash>(())),
        Err(RequestError::Exited)
    );
    assert_eq!(block_on(hanging), Err(RequestError::Exited));
    let ServerEvent::Exited { reason } =
        wait_event(&mut events, |e| matches!(e, ServerEvent::Exited { .. }))
    else {
        unreachable!()
    };
    assert!(
        reason.contains('3') && reason.contains("fatal: boom"),
        "{reason}"
    );
}

#[test]
fn dropping_the_last_handle_kills_the_server() {
    let fake = Fake::new();
    let (server, mut events) = fake.start(&[]);
    assert!(matches!(next_event(&mut events), ServerEvent::Initialized));
    let pid = fake.pid();
    let clone = server.clone();
    drop(server);
    assert!(process_alive(pid), "a clone still holds it");
    drop(clone);
    wait_dead(pid);
    assert!(matches!(
        wait_event(&mut events, |e| matches!(e, ServerEvent::Exited { .. })),
        ServerEvent::Exited { ref reason } if reason == "stopped"
    ));
}

#[test]
fn a_failed_initialize_is_reported_as_exit() {
    let fake = Fake::new();
    let (server, mut events) = fake.start(&["--fail-init"]);
    let ServerEvent::Exited { reason } = next_event(&mut events) else {
        panic!("expected Exited first");
    };
    assert_eq!(reason, "initialize failed: no workspace");
    assert!(server.capabilities().is_none());
    wait_dead(fake.pid());
}

#[test]
fn a_missing_server_is_not_found() {
    let config = ServerConfig {
        command: "flux-no-such-language-server".into(),
        ..Fake::new().config(&[])
    };
    let error = LanguageServer::start(&config, Path::new("/")).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn uris_from_the_server_compare_by_path() {
    // A server may escape differently: compare paths, not strings.
    let a = Uri::from_str("file:///tmp/a%20b/c.rs").unwrap();
    let b = flux_lsp::position::uri_from_path(Path::new("/tmp/a b/c.rs"));
    assert_eq!(
        flux_lsp::position::path_from_uri(&a),
        flux_lsp::position::path_from_uri(&b)
    );
}
