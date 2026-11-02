//! The client against a fake server (`tests/fake_server.py`, needs `python3`): lifecycle, requests,
//! cancellation, notifications both ways, server requests, crashes.

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::thread;
use std::time::{Duration, Instant};

use flux_lsp::lsp_types::FileChangeType;
use flux_lsp::lsp_types::notification::{DidOpenTextDocument, Notification};
use flux_lsp::lsp_types::request::{Completion, HoverRequest, Request};
use flux_lsp::lsp_types::{
    CompletionParams, DidOpenTextDocumentParams, HoverContents, HoverParams, MarkupContent,
    MessageType, Position, TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams,
    Uri,
};
use flux_lsp::{Health, LanguageServer, RequestError, ServerConfig, ServerEvent, ServerStatus};
use futures::channel::mpsc::{TryRecvError, UnboundedReceiver};
use serde_json::{Value, json};

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
            file_patterns: Vec::new(),
            initialization_options: None,
            settings: None,
            install: None,
            env: Default::default(),
            secondary: false,
            activation: None,
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
    flux_platform::process::alive(pid)
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

    // After `initialized` the server asks to apply an edit (the window answers it), reports
    // progress and a message; the queued didOpen went after `initialized` too, and the server
    // answered it with diagnostics. (`window/logMessage` in between is not an event.)
    let ServerEvent::ApplyEdit(apply) = next_event(&mut events) else {
        panic!("the applyEdit request comes as an event");
    };
    apply.respond(true, None);
    // Answered once: a second answer (or the drop) writes nothing.
    apply.respond(false, None);
    drop(apply);
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
    assert_eq!(response(102.into())["result"]["applied"], true);
    let applies = fake
        .received()
        .iter()
        .filter(|m| m["id"] == 102 && m.get("method").is_none())
        .count();
    assert_eq!(applies, 1);

    // An applyEdit request nobody answers is answered "not applied" when dropped.
    server.notify::<AskApply>(());
    let ServerEvent::ApplyEdit(apply) =
        wait_event(&mut events, |e| matches!(e, ServerEvent::ApplyEdit(_)))
    else {
        unreachable!()
    };
    assert_eq!(apply.params.label.as_deref(), Some("dropped"));
    drop(apply);
    assert_eq!(response(103.into())["result"]["applied"], false);

    // The client offers what code actions and workspace edits need.
    let capabilities = &initialize["params"]["capabilities"];
    assert_eq!(capabilities["workspace"]["applyEdit"], true);
    assert_eq!(
        capabilities["textDocument"]["codeAction"]["resolveSupport"]["properties"],
        serde_json::json!(["edit"])
    );

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

/// A custom notification: the fake server asks to apply an edit, and nobody answers.
enum AskApply {}

impl flux_lsp::lsp_types::notification::Notification for AskApply {
    type Params = ();
    const METHOD: &'static str = "flux/askApply";
}

/// Custom notifications: the fake server sends rust-analyzer's status, (un)registers watched files
/// with the params given.
enum FakeStatus {}

impl flux_lsp::lsp_types::notification::Notification for FakeStatus {
    type Params = Value;
    const METHOD: &'static str = "flux/status";
}

enum FakeRegister {}

impl flux_lsp::lsp_types::notification::Notification for FakeRegister {
    type Params = Value;
    const METHOD: &'static str = "flux/register";
}

enum FakeUnregister {}

impl flux_lsp::lsp_types::notification::Notification for FakeUnregister {
    type Params = Value;
    const METHOD: &'static str = "flux/unregister";
}

#[test]
fn server_status_watched_files_and_the_log() {
    let fake = Fake::new();
    let root = fake.dir.path().to_path_buf();
    let (server, mut events) = fake.start(&[]);
    assert!(matches!(next_event(&mut events), ServerEvent::Initialized));
    let initialize = &fake.received()[1];
    let capabilities = &initialize["params"]["capabilities"];
    assert_eq!(
        capabilities["experimental"]["serverStatusNotification"],
        true
    );
    assert_eq!(
        capabilities["workspace"]["didChangeWatchedFiles"],
        json!({ "dynamicRegistration": true, "relativePatternSupport": true })
    );

    // rust-analyzer couldn't load the project.
    server.notify::<FakeStatus>(json!({
        "health": "error", "quiescent": true, "message": "Failed to load workspaces.",
    }));
    let ServerEvent::Status(status) =
        wait_event(&mut events, |e| matches!(e, ServerEvent::Status(_)))
    else {
        unreachable!()
    };
    assert_eq!(
        status,
        ServerStatus {
            health: Health::Error,
            quiescent: true,
            message: Some("Failed to load workspaces.".into()),
        }
    );

    // Nothing is watched until the server registers globs.
    assert!(!server.watches_files());
    assert_eq!(
        server.files_changed(&[(root.join("src/a.rs"), FileChangeType::CHANGED)]),
        0
    );
    let base = flux_lsp::position::uri_from_path(&root);
    server.notify::<FakeRegister>(json!({ "registrations": [{
        "id": "watch",
        "method": "workspace/didChangeWatchedFiles",
        "registerOptions": { "watchers": [
            { "globPattern": { "baseUri": base.as_str(), "pattern": "**/*.rs" } },
        ]},
    }]}));
    wait_event(&mut events, |e| matches!(e, ServerEvent::WatchersChanged));
    assert!(server.watches_files());
    fake.wait_received(|m| m["id"] == "reg" && m.get("method").is_none());
    let sent = server.files_changed(&[
        (root.join("src/a.rs"), FileChangeType::CREATED),
        (root.join("README.md"), FileChangeType::CHANGED),
    ]);
    assert_eq!(sent, 1);
    let notification = fake.wait_received(|m| m["method"] == "workspace/didChangeWatchedFiles");
    let uri = flux_lsp::position::uri_from_path(&root.join("src/a.rs"));
    assert_eq!(
        notification["params"]["changes"],
        json!([{ "uri": uri.as_str(), "type": 1 }])
    );

    server.notify::<FakeUnregister>(json!({ "unregisterations": [{
        "id": "watch", "method": "workspace/didChangeWatchedFiles",
    }]}));
    wait_event(&mut events, |e| matches!(e, ServerEvent::WatchersChanged));
    assert!(!server.watches_files());

    // The log: what Flux noted, the server's `window/logMessage` (sent after `initialized`) and
    // its status.
    let log = server.log().text();
    assert!(log.contains("flux    started python3"), "{log}");
    assert!(log.contains("log     ignored"), "{log}");
    assert!(
        log.contains("status: error Failed to load workspaces."),
        "{log}"
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
    // The log outlives the process: its stderr and the exit are there.
    let log = server.log().text();
    assert!(log.contains("stderr  fatal: boom"), "{log}");
    assert!(log.contains("flux    exited: "), "{log}");
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

#[test]
fn settings_change_while_the_server_runs() {
    let fake = Fake::new();
    let config = ServerConfig {
        settings: Some(serde_json::json!({ "a": { "x": 1 } })),
        env: [("FLUX_FAKE_ENV".to_string(), "on".to_string())].into(),
        ..fake.config(&[])
    };
    let (server, mut events) = LanguageServer::start(&config, fake.dir.path()).unwrap();
    assert!(matches!(next_event(&mut events), ServerEvent::Initialized));
    // The user's environment reached the process.
    assert_eq!(fake.received()[0]["env"], "on");
    // The answer to the first `workspace/configuration` (sections "a" and "b").
    let answer = |id: &str| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(found) = fake.received().into_iter().find(|m| m["id"] == id) {
                return found["result"].clone();
            }
            assert!(Instant::now() < deadline, "no answer to {id}");
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    assert_eq!(answer("conf-1"), serde_json::json!([{ "x": 1 }, null]));

    server.set_settings(serde_json::json!({ "a": { "x": 2 }, "b": true }));
    server.notify::<FluxAskConfiguration>(());
    assert_eq!(answer("conf-2"), serde_json::json!([{ "x": 2 }, 2, true]));
    let changed = fake
        .received()
        .into_iter()
        .find(|m| m["method"] == "workspace/didChangeConfiguration")
        .expect("didChangeConfiguration sent");
    assert_eq!(
        changed["params"]["settings"],
        serde_json::json!({ "a": { "x": 2 }, "b": true })
    );
    drop(server);
}

/// The fake server's request to ask for its settings again.
enum FluxAskConfiguration {}

impl Notification for FluxAskConfiguration {
    type Params = ();
    const METHOD: &'static str = "flux/askConfiguration";
}

/// The fake server's next pull answers `ServerCancelled`.
enum FluxCancelNextPull {}

impl Notification for FluxCancelNextPull {
    type Params = ();
    const METHOD: &'static str = "flux/cancelNextPull";
}

/// The fake server sends `workspace/diagnostic/refresh`.
enum FluxRefreshDiagnostics {}

impl Notification for FluxRefreshDiagnostics {
    type Params = ();
    const METHOD: &'static str = "flux/refreshDiagnostics";
}

#[test]
fn pulled_diagnostics_with_result_ids_cancellation_and_refresh() {
    use flux_lsp::lsp_types::notification::DidChangeTextDocument;
    use flux_lsp::lsp_types::{
        DidChangeTextDocumentParams, TextDocumentContentChangeEvent,
        VersionedTextDocumentIdentifier,
    };
    use flux_lsp::pull::PulledReport;

    let fake = Fake::new();
    let (server, mut events) = fake.start(&["--pull"]);
    assert!(matches!(next_event(&mut events), ServerEvent::Initialized));
    // The client says it can pull, with related documents, and be told to pull again.
    let capabilities = &fake.received()[1]["params"]["capabilities"];
    assert_eq!(
        capabilities["textDocument"]["diagnostic"],
        json!({ "dynamicRegistration": false, "relatedDocumentSupport": true })
    );
    assert_eq!(
        capabilities["workspace"]["diagnostics"],
        json!({ "refreshSupport": true })
    );
    assert!(server.pulls_diagnostics());
    server.notify::<DidOpenTextDocument>(DidOpenTextDocumentParams {
        text_document: TextDocumentItem::new(uri(&fake), "rust".into(), 0, "fn main() {}".into()),
    });

    let pulled = block_on(server.pull_diagnostics(uri(&fake), None)).unwrap();
    let Some(PulledReport::Full { result_id, items }) = pulled.report else {
        panic!("a full report first");
    };
    assert_eq!(result_id.as_deref(), Some("r0"));
    assert_eq!(items[0].message, "pulled 0");
    // The server's identifier went with the request.
    assert_eq!(items[0].source.as_deref(), Some("fake"));
    assert_eq!(pulled.related.len(), 1);
    assert!(pulled.related[0].0.as_str().ends_with("/related.rs"));

    // Nothing changed: "unchanged" for the result id named.
    let pulled = block_on(server.pull_diagnostics(uri(&fake), Some("r0".into()))).unwrap();
    assert_eq!(
        pulled.report,
        Some(PulledReport::Unchanged {
            result_id: "r0".into()
        })
    );
    let request = fake.wait_received(|m| {
        m["method"] == "textDocument/diagnostic" && m["params"]["previousResultId"] == "r0"
    });
    assert_eq!(request["params"]["identifier"], "fake");

    // An edit: a new report.
    server.notify::<DidChangeTextDocument>(DidChangeTextDocumentParams {
        text_document: VersionedTextDocumentIdentifier::new(uri(&fake), 1),
        content_changes: vec![TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: "fn main() { x }".into(),
        }],
    });
    let pulled = block_on(server.pull_diagnostics(uri(&fake), Some("r0".into()))).unwrap();
    assert!(matches!(
        pulled.report,
        Some(PulledReport::Full { ref result_id, ref items })
            if result_id.as_deref() == Some("r1") && items[0].message == "pulled 1"
    ));

    // The server cancels a pull and asks for it again.
    server.notify::<FluxCancelNextPull>(());
    let error = block_on(server.pull_diagnostics(uri(&fake), Some("r1".into()))).unwrap_err();
    assert!(error.retrigger(), "{error:?}");

    // The server's configuration changed: it asks the client to pull again.
    server.notify::<FluxRefreshDiagnostics>(());
    wait_event(&mut events, |e| {
        matches!(e, ServerEvent::DiagnosticsRefresh)
    });
    let answer = fake.wait_received(|m| m["id"] == "refresh" && m.get("method").is_none());
    assert_eq!(answer["result"], Value::Null);
}

#[test]
fn completion_and_hover_of_a_main_and_a_secondary_server_merge() {
    use flux_lsp::lsp_types::{CompletionResponse, CompletionTextEdit, TextEdit};

    let main_fake = Fake::new();
    let secondary_fake = Fake::new();
    let (main, mut main_events) = main_fake.start(&["--items=flex,grid", "--hover=**main**"]);
    let (secondary, mut secondary_events) =
        secondary_fake.start(&["--items=flex,flex-col", "--hover=`.flex`"]);
    assert!(matches!(
        next_event(&mut main_events),
        ServerEvent::Initialized
    ));
    assert!(matches!(
        next_event(&mut secondary_events),
        ServerEvent::Initialized
    ));
    let completion = |server: &LanguageServer, fake: &Fake| {
        block_on(server.request::<Completion>(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri(fake) },
                position: Position::new(0, 0),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        }))
        .unwrap()
        .unwrap()
    };
    let answers = vec![
        (0, completion(&main, &main_fake)),
        (1, completion(&secondary, &secondary_fake)),
    ];
    let (items, incomplete) = flux_lsp::merge::completions(answers);
    let labels: Vec<(usize, &str)> = items
        .iter()
        .map(|(source, item)| (*source, item.label.as_str()))
        .collect();
    // The main server's first; the secondary's `flex` is the same item.
    assert_eq!(labels, [(0, "flex"), (0, "grid"), (1, "flex-col")]);
    assert!(!incomplete);
    // An item that inserts other text is kept.
    let edited = CompletionResponse::Array(vec![flux_lsp::lsp_types::CompletionItem {
        label: "flex".into(),
        text_edit: Some(CompletionTextEdit::Edit(TextEdit::new(
            Default::default(),
            "flex ".into(),
        ))),
        ..Default::default()
    }]);
    let (items, _) =
        flux_lsp::merge::completions(vec![(0, completion(&main, &main_fake)), (1, edited)]);
    assert_eq!(items.len(), 3);

    let hover = |server: &LanguageServer, fake: &Fake| {
        block_on(server.request::<HoverRequest>(hover_params(uri(fake), 0))).unwrap()
    };
    let merged = flux_lsp::merge::hovers(
        [hover(&main, &main_fake), hover(&secondary, &secondary_fake)]
            .into_iter()
            .flatten()
            .collect(),
    )
    .unwrap();
    assert_eq!(
        merged.contents,
        HoverContents::Markup(MarkupContent {
            kind: flux_lsp::lsp_types::MarkupKind::Markdown,
            value: "**main**\n\n---\n\n`.flex`".into(),
        })
    );
    // A server without a hover there adds nothing.
    let quiet_fake = Fake::new();
    let (quiet, mut quiet_events) = quiet_fake.start(&["--hover="]);
    assert!(matches!(
        next_event(&mut quiet_events),
        ServerEvent::Initialized
    ));
    assert_eq!(hover(&quiet, &quiet_fake), None);
    let merged = flux_lsp::merge::hovers(
        [hover(&main, &main_fake), hover(&quiet, &quiet_fake)]
            .into_iter()
            .flatten()
            .collect(),
    )
    .unwrap();
    assert_eq!(
        merged.contents,
        HoverContents::Markup(MarkupContent {
            kind: flux_lsp::lsp_types::MarkupKind::Markdown,
            value: "**main**".into(),
        })
    );
}
