//! A running language server: lifecycle, requests and responses, notifications, server events.
//!
//! Three background threads per server: a writer (our messages go to stdin in call order, so a
//! server that doesn't read never blocks the caller), a reader (responses resolve their futures,
//! notifications become [`ServerEvent`]s, server requests are answered right there), and a stderr
//! reader. The process belongs to the [`LanguageServer`] handles: when the last one is dropped it
//! is killed. Set `FLUX_LSP_LOG=1` to log the traffic and the server's stderr to our stderr.

use std::collections::HashMap;
use std::fmt;
use std::io::{self, BufReader};
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{self as std_mpsc, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures::channel::oneshot;
use lsp_types::notification::Notification;
use lsp_types::request::Request;
use lsp_types::{
    InitializeResult, NumberOrString, ProgressParams, ProgressParamsValue,
    PublishDiagnosticsParams, ServerCapabilities, ShowMessageParams, WorkDoneProgress,
};
use serde_json::{Value, json};

use crate::config::{self, ServerConfig};
use crate::position::uri_from_path;
use crate::transport::{self, Stderr};

/// How long `shutdown` waits for the server's answer, and then for the process to exit.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const EXIT_TIMEOUT: Duration = Duration::from_secs(1);
/// JSON-RPC: the method is not known.
const METHOD_NOT_FOUND: i64 = -32601;

/// What a server tells us without being asked.
#[derive(Debug, Clone)]
pub enum ServerEvent {
    /// `initialize` answered and `initialized` sent: documents can be opened, requests made.
    Initialized,
    Diagnostics(PublishDiagnosticsParams),
    /// `$/progress` (work done): `title` from begin, `message`/`percentage` from report; `done`
    /// on end.
    Progress {
        token: String,
        title: Option<String>,
        message: Option<String>,
        percentage: Option<u32>,
        done: bool,
    },
    /// `window/showMessage` (errors and warnings worth showing).
    Message {
        kind: lsp_types::MessageType,
        text: String,
    },
    /// The process exited or the connection broke; `reason` is for the status bar (last stderr
    /// line or exit status). Later requests fail with [`RequestError::Exited`].
    Exited {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum RequestError {
    /// The server answered with an error (`code`, `message`); `ContentModified` and
    /// `RequestCancelled` included.
    Server { code: i64, message: String },
    /// The server is gone.
    Exited,
    /// The response didn't fit the request's result type.
    Decode(String),
    /// Not initialized yet.
    NotReady,
}

impl RequestError {
    /// `ContentModified` (-32801) and `RequestCancelled` (-32800): the answer is outdated rather
    /// than wrong; ask again after the next edit, don't report it.
    pub fn is_outdated(&self) -> bool {
        matches!(
            self,
            Self::Server {
                code: -32801 | -32800,
                ..
            }
        )
    }
}

impl fmt::Display for RequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Server { message, .. } => f.write_str(message),
            Self::Exited => f.write_str("the language server has exited"),
            Self::Decode(error) => write!(f, "unexpected response: {error}"),
            Self::NotReady => f.write_str("the language server is starting"),
        }
    }
}

impl std::error::Error for RequestError {}

/// A handle to a running server; cheap to clone, usable from any thread.
#[derive(Clone)]
pub struct LanguageServer {
    shared: Arc<Shared>,
    /// Kills the process when the last handle is gone.
    process: Arc<ProcessGuard>,
}

impl fmt::Debug for LanguageServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LanguageServer")
            .field("name", &self.shared.name)
            .finish_non_exhaustive()
    }
}

impl LanguageServer {
    /// Spawns the server with `root` as the workspace folder and starts `initialize` in the
    /// background (we advertise UTF-16 positions only). Server events arrive on the receiver;
    /// the first one is [`ServerEvent::Initialized`] or [`ServerEvent::Exited`].
    ///
    /// Until then requests fail with [`RequestError::NotReady`] and notifications are queued
    /// (sent right after `initialized`, in order). Fails if the command is not installed
    /// (`ErrorKind::NotFound`) or can't be started.
    pub fn start(
        config: &ServerConfig,
        root: &Path,
    ) -> io::Result<(Self, UnboundedReceiver<ServerEvent>)> {
        let program = config.resolve_command().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} is not installed", config.command),
            )
        })?;
        // The server runs `cargo`, `go`, `node`: it gets our search path, with Flux's Node.js first
        // for an npm server Flux installed.
        let launch = config::launch(config, root, &program);
        let mut child = Command::new(program)
            .args(&config.args)
            .current_dir(root)
            .env("PATH", &launch.path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            return Err(io::Error::other("no pipes to the language server"));
        };

        let log = std::env::var_os("FLUX_LSP_LOG").is_some_and(|v| !v.is_empty() && v != "0");
        let (events, receiver) = unbounded();
        let (writer, messages) = std_mpsc::channel();
        transport::spawn_writer(&config.name, stdin, messages);
        let stderr = transport::spawn_stderr(&config.name, stderr, log);
        let shared = Arc::new(Shared {
            name: config.name.clone(),
            settings: launch.settings,
            root_uri: uri_from_path(root),
            root_name: root
                .file_name()
                .map_or_else(|| root.to_string_lossy(), |name| name.to_string_lossy())
                .into_owned(),
            writer,
            events,
            state: Mutex::new(State {
                phase: Phase::Starting,
                capabilities: None,
                pending: HashMap::new(),
                queued: Vec::new(),
                failure: None,
            }),
            next_id: AtomicI64::new(1),
            child: Mutex::new(Some(child)),
            stderr,
            log,
        });
        let reader = shared.clone();
        let spawned = thread::Builder::new()
            .name(format!("lsp-{}-reader", config.name))
            .spawn(move || reader.read(stdout));
        if let Err(err) = spawned {
            shared.kill();
            return Err(err);
        }

        let params = initialize_params(
            launch.initialization_options.as_ref(),
            root,
            &shared.root_uri,
            &shared.root_name,
        );
        let initialized = shared.clone();
        shared.send_request(
            "initialize",
            params,
            Box::new(move |result| initialized.initialize_answered(result)),
            Phase::Starting,
        );
        let process = Arc::new(ProcessGuard {
            shared: shared.clone(),
        });
        Ok((Self { shared, process }, receiver))
    }

    pub fn name(&self) -> &str {
        &self.shared.name
    }

    /// After [`ServerEvent::Initialized`].
    pub fn capabilities(&self) -> Option<Arc<ServerCapabilities>> {
        self.shared.state().capabilities.clone()
    }

    /// Sends a request. Dropping the future before it resolves sends `$/cancelRequest`.
    pub fn request<R: Request>(
        &self,
        params: R::Params,
    ) -> impl Future<Output = Result<R::Result, RequestError>> + Send + use<R> {
        let (sender, receiver) = oneshot::channel();
        let respond: Callback = Box::new(move |result| {
            let _ = sender.send(result);
        });
        let id = match serde_json::to_value(params) {
            Ok(params) => self
                .shared
                .send_request(R::METHOD, params, respond, Phase::Ready),
            Err(err) => {
                respond(Err(RequestError::Decode(err.to_string())));
                None
            }
        };
        let cancel = CancelOnDrop {
            shared: self.shared.clone(),
            id,
        };
        async move {
            let result = receiver.await.unwrap_or(Err(RequestError::Exited));
            drop(cancel);
            serde_json::from_value(result?).map_err(|err| RequestError::Decode(err.to_string()))
        }
    }

    /// Sends a notification; before initialization it is queued, after exit it is dropped.
    pub fn notify<N: Notification>(&self, params: N::Params) {
        match serde_json::to_value(params) {
            Ok(params) => self.shared.notify(N::METHOD, params),
            Err(err) => eprintln!("flux-lsp: can't encode {}: {err}", N::METHOD),
        }
    }

    /// `shutdown` + `exit`; kills the process if it doesn't exit within a second or two. Starts
    /// right away, even if the future is never awaited; it resolves when the process is gone.
    pub fn shutdown(&self) -> impl Future<Output = ()> + Send + use<> {
        let (done, finished) = oneshot::channel();
        let shared = self.shared.clone();
        // The process stays ours until the shutdown is over, even if every handle is dropped.
        let process = self.process.clone();
        let spawned = thread::Builder::new()
            .name(format!("lsp-{}-shutdown", self.shared.name))
            .spawn(move || {
                shared.shutdown();
                drop(process);
                let _ = done.send(());
            });
        if spawned.is_err() {
            self.shared.kill();
        }
        async move {
            let _ = finished.await;
        }
    }
}

/// Called once with the response (or the reason there is none).
type Callback = Box<dyn FnOnce(Result<Value, RequestError>) + Send>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Phase {
    /// `initialize` sent, no answer yet.
    Starting,
    Ready,
    /// `shutdown` sent or the handles are gone: no new requests.
    Stopping,
    Exited,
}

struct State {
    phase: Phase,
    capabilities: Option<Arc<ServerCapabilities>>,
    /// Requests waiting for a response, by id.
    pending: HashMap<i64, Callback>,
    /// Notifications sent before initialization, already framed.
    queued: Vec<Vec<u8>>,
    /// Why the server is being stopped by us (a failed `initialize`): the exit reason.
    failure: Option<String>,
}

struct Shared {
    name: String,
    /// Answers to `workspace/configuration`, by section.
    settings: Value,
    root_uri: lsp_types::Uri,
    root_name: String,
    writer: Sender<Vec<u8>>,
    events: UnboundedSender<ServerEvent>,
    state: Mutex<State>,
    next_id: AtomicI64,
    /// `None` once killed by us.
    child: Mutex<Option<Child>>,
    stderr: Arc<Stderr>,
    log: bool,
}

/// Kills the process when the last [`LanguageServer`] is dropped.
struct ProcessGuard {
    shared: Arc<Shared>,
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        self.shared.kill();
    }
}

/// Cancels a request whose future is dropped before the response: forgets the callback and tells
/// the server. A no-op once the response has arrived.
struct CancelOnDrop {
    shared: Arc<Shared>,
    id: Option<i64>,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        let Some(id) = self.id else {
            return;
        };
        let mut state = self.shared.state();
        if state.pending.remove(&id).is_some() && state.phase == Phase::Ready {
            self.shared
                .write(&notification("$/cancelRequest", json!({ "id": id })));
        }
    }
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self, message: &Value) {
        let body = serde_json::to_vec(message).expect("JSON values serialize");
        if self.log {
            eprintln!("[lsp {}] --> {}", self.name, abbreviate(&body));
        }
        let _ = self.writer.send(transport::frame(&body));
    }

    fn emit(&self, event: ServerEvent) {
        let _ = self.events.unbounded_send(event);
    }

    /// Registers the callback and sends the request if the server is ready, or in the `also`
    /// phase (`Starting` for `initialize`, `Stopping` for `shutdown`; `Ready` for the rest);
    /// otherwise calls the callback with the reason right away. The id if sent.
    fn send_request(
        &self,
        method: &str,
        params: Value,
        callback: Callback,
        also: Phase,
    ) -> Option<i64> {
        let mut state = self.state();
        let refused = match state.phase {
            Phase::Ready => None,
            phase if phase == also && phase != Phase::Exited => None,
            Phase::Starting => Some(RequestError::NotReady),
            Phase::Stopping | Phase::Exited => Some(RequestError::Exited),
        };
        if let Some(error) = refused {
            drop(state);
            callback(Err(error));
            return None;
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        state.pending.insert(id, callback);
        let mut message = json!({ "jsonrpc": "2.0", "id": id, "method": method });
        if !params.is_null() {
            message["params"] = params;
        }
        // Under the lock: requests and notifications reach the writer in call order.
        self.write(&message);
        Some(id)
    }

    fn notify(&self, method: &str, params: Value) {
        let message = notification(method, params);
        let mut state = self.state();
        match state.phase {
            Phase::Starting => {
                let body = serde_json::to_vec(&message).expect("JSON values serialize");
                state.queued.push(body);
            }
            Phase::Ready => self.write(&message),
            Phase::Stopping | Phase::Exited => {}
        }
    }

    /// The answer to `initialize`: on success, `initialized` goes out before the queued
    /// notifications, and then [`ServerEvent::Initialized`].
    fn initialize_answered(&self, result: Result<Value, RequestError>) {
        let reason = match result {
            Ok(value) => match serde_json::from_value::<InitializeResult>(value) {
                Ok(init) => {
                    let mut state = self.state();
                    if state.phase != Phase::Starting {
                        return;
                    }
                    state.capabilities = Some(Arc::new(init.capabilities));
                    self.write(&notification("initialized", json!({})));
                    for body in std::mem::take(&mut state.queued) {
                        if self.log {
                            eprintln!("[lsp {}] --> {}", self.name, abbreviate(&body));
                        }
                        let _ = self.writer.send(transport::frame(&body));
                    }
                    state.phase = Phase::Ready;
                    drop(state);
                    self.emit(ServerEvent::Initialized);
                    return;
                }
                Err(err) => format!("unexpected initialize response: {err}"),
            },
            Err(RequestError::Server { message, .. }) => format!("initialize failed: {message}"),
            // The process is gone: the reader reports why.
            Err(_) => return,
        };
        self.state().failure = Some(reason);
        self.kill();
    }

    /// The reader thread: until stdout ends.
    fn read(self: Arc<Self>, stdout: ChildStdout) {
        let mut reader = BufReader::new(stdout);
        loop {
            match transport::read_message(&mut reader) {
                Ok(Some(body)) => self.dispatch(&body),
                Ok(None) => break,
                Err(err) => {
                    if self.log {
                        eprintln!("[lsp {}] read error: {err}", self.name);
                    }
                    break;
                }
            }
        }
        self.exited();
    }

    fn dispatch(&self, body: &[u8]) {
        if self.log {
            eprintln!("[lsp {}] <-- {}", self.name, abbreviate(body));
        }
        let Ok(mut message) = serde_json::from_slice::<Value>(body) else {
            eprintln!("flux-lsp: {} sent invalid JSON", self.name);
            return;
        };
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let id = message.get_mut("id").map(Value::take);
        let params = message.get_mut("params").map_or(Value::Null, Value::take);
        match (method, id) {
            (Some(method), Some(id)) => self.answer(id, &method, params),
            (Some(method), None) => self.notification(&method, params),
            (None, Some(id)) => {
                let result = match message.get_mut("error") {
                    Some(error) => Err(RequestError::Server {
                        code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                        message: error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("error")
                            .to_owned(),
                    }),
                    None => Ok(message.get_mut("result").map_or(Value::Null, Value::take)),
                };
                let id = match id {
                    Value::Number(n) => n.as_i64(),
                    Value::String(s) => s.parse().ok(),
                    _ => None,
                };
                let callback = id.and_then(|id| self.state().pending.remove(&id));
                if let Some(callback) = callback {
                    callback(result);
                }
            }
            (None, None) => {}
        }
    }

    fn notification(&self, method: &str, params: Value) {
        match method {
            "textDocument/publishDiagnostics" => {
                if let Ok(params) = serde_json::from_value(params) {
                    self.emit(ServerEvent::Diagnostics(params));
                }
            }
            "$/progress" => {
                // Only work done progress: partial results are never asked for.
                if let Ok(ProgressParams {
                    token,
                    value: ProgressParamsValue::WorkDone(progress),
                }) = serde_json::from_value(params)
                {
                    self.emit(progress_event(token, progress));
                }
            }
            "window/showMessage" => {
                if let Ok(ShowMessageParams { typ, message }) = serde_json::from_value(params) {
                    self.emit(ServerEvent::Message {
                        kind: typ,
                        text: message,
                    });
                }
            }
            _ => {}
        }
    }

    /// Answers a request from the server.
    fn answer(&self, id: Value, method: &str, params: Value) {
        let result = match method {
            // Each item asks for a section of the settings; `null` — the server's default.
            "workspace/configuration" => {
                let items = params["items"].as_array().map_or(&[][..], Vec::as_slice);
                let answers = items
                    .iter()
                    .map(|item| config::settings_section(&self.settings, item["section"].as_str()))
                    .collect();
                Ok(Value::Array(answers))
            }
            "workspace/workspaceFolders" => Ok(json!([
                { "uri": self.root_uri, "name": self.root_name }
            ])),
            "client/registerCapability"
            | "client/unregisterCapability"
            | "window/workDoneProgress/create" => Ok(Value::Null),
            "workspace/applyEdit" => Ok(json!({
                "applied": false,
                "failureReason": "Flux does not apply edits requested by the server",
            })),
            "window/showMessageRequest" => {
                if let Ok(ShowMessageParams { typ, message }) = serde_json::from_value(params) {
                    self.emit(ServerEvent::Message {
                        kind: typ,
                        text: message,
                    });
                }
                // No action chosen.
                Ok(Value::Null)
            }
            "window/showDocument" => Ok(json!({ "success": false })),
            // "Refresh your semantic tokens, inlay hints, …": we have none.
            method if method.starts_with("workspace/") && method.ends_with("/refresh") => {
                Ok(Value::Null)
            }
            method => Err(format!("{method} is not supported")),
        };
        let response = match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(message) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": METHOD_NOT_FOUND, "message": message },
            }),
        };
        self.write(&response);
    }

    /// The connection is over: pending requests fail, and the exit is reported once.
    fn exited(&self) {
        let (pending, failure) = {
            let mut state = self.state();
            state.phase = Phase::Exited;
            state.queued.clear();
            (std::mem::take(&mut state.pending), state.failure.take())
        };
        for callback in pending.into_values() {
            callback(Err(RequestError::Exited));
        }
        let reason = failure.unwrap_or_else(|| self.exit_reason());
        self.emit(ServerEvent::Exited { reason });
    }

    /// The exit status and the last thing the server said on stderr.
    fn exit_reason(&self) -> String {
        let deadline = Instant::now() + EXIT_TIMEOUT;
        let status = loop {
            let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
            let Some(process) = child.as_mut() else {
                break None;
            };
            match process.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < deadline => {}
                // Closed stdout but still running: of no use any more.
                _ => {
                    let _ = process.kill();
                    break process.wait().ok();
                }
            }
            drop(child);
            thread::sleep(Duration::from_millis(10));
        };
        // The last lines may still be on their way.
        let deadline = Instant::now() + Duration::from_millis(300);
        while !self.stderr.closed.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let last_line = self
            .stderr
            .lines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .back()
            .cloned();
        match (status, last_line) {
            (None, _) => "stopped".to_string(),
            (Some(status), None) => status.to_string(),
            (Some(status), Some(line)) => format!("{status}: {}", truncate(&line, 200)),
        }
    }

    /// The `shutdown` request, `exit`, and a kill if the process lingers.
    fn shutdown(&self) {
        let was = {
            let mut state = self.state();
            let was = state.phase;
            if was < Phase::Stopping {
                state.phase = Phase::Stopping;
            }
            was
        };
        if was >= Phase::Stopping {
            return;
        }
        if was == Phase::Ready {
            let (answered, answer) = std_mpsc::channel();
            let callback: Callback = Box::new(move |result| {
                let _ = answered.send(result);
            });
            if self
                .send_request("shutdown", Value::Null, callback, Phase::Stopping)
                .is_some()
            {
                let _ = answer.recv_timeout(SHUTDOWN_TIMEOUT);
            }
        }
        self.write(&notification("exit", Value::Null));
        let deadline = Instant::now() + EXIT_TIMEOUT;
        loop {
            let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
            let Some(process) = child.as_mut() else {
                return;
            };
            match process.try_wait() {
                Ok(None) if Instant::now() < deadline => {}
                Ok(Some(_)) => return,
                _ => {
                    let _ = process.kill();
                    let _ = process.wait();
                    return;
                }
            }
            drop(child);
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Kills the process (if it's still ours) and reaps it in the background.
    fn kill(&self) {
        {
            let mut state = self.state();
            if state.phase < Phase::Stopping {
                state.phase = Phase::Stopping;
            }
        }
        let child = self.child.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = thread::Builder::new()
                .name(format!("lsp-{}-reaper", self.name))
                .spawn(move || {
                    let _ = child.wait();
                });
        }
    }
}

/// Like `Shared::send_request`, the `params` are omitted when null: JSON-RPC wants an object or
/// an array there.
fn notification(method: &str, params: Value) -> Value {
    let mut message = json!({ "jsonrpc": "2.0", "method": method });
    if !params.is_null() {
        message["params"] = params;
    }
    message
}

fn progress_event(token: NumberOrString, progress: WorkDoneProgress) -> ServerEvent {
    let token = match token {
        NumberOrString::Number(n) => n.to_string(),
        NumberOrString::String(s) => s,
    };
    match progress {
        WorkDoneProgress::Begin(begin) => ServerEvent::Progress {
            token,
            title: Some(begin.title),
            message: begin.message,
            percentage: begin.percentage,
            done: false,
        },
        WorkDoneProgress::Report(report) => ServerEvent::Progress {
            token,
            title: None,
            message: report.message,
            percentage: report.percentage,
            done: false,
        },
        WorkDoneProgress::End(end) => ServerEvent::Progress {
            token,
            title: None,
            message: end.message,
            percentage: None,
            done: true,
        },
    }
}

fn initialize_params(
    initialization_options: Option<&Value>,
    root: &Path,
    root_uri: &lsp_types::Uri,
    root_name: &str,
) -> Value {
    let mut params = json!({
        "processId": std::process::id(),
        "clientInfo": { "name": "Flux", "version": env!("CARGO_PKG_VERSION") },
        "rootPath": root.to_string_lossy(),
        "rootUri": root_uri,
        "workspaceFolders": [{ "uri": root_uri, "name": root_name }],
        "capabilities": client_capabilities(),
        "trace": "off",
    });
    if let Some(options) = initialization_options {
        params["initializationOptions"] = options.clone();
    }
    params
}

/// What flux-app can do with a server's answers. Positions are UTF-16 only; edits from the server
/// are applied by us (no `workspace/applyEdit`), without file operations; file changes are watched
/// by the servers themselves (no `didChangeWatchedFiles`).
fn client_capabilities() -> Value {
    json!({
        "general": {
            "positionEncodings": ["utf-16"],
        },
        "workspace": {
            "applyEdit": false,
            "configuration": true,
            "workspaceFolders": true,
            "workspaceEdit": {
                "documentChanges": true,
                "resourceOperations": [],
                "failureHandling": "abort",
            },
            "didChangeConfiguration": { "dynamicRegistration": false },
            "didChangeWatchedFiles": { "dynamicRegistration": false },
        },
        "textDocument": {
            "synchronization": {
                "dynamicRegistration": false,
                "willSave": false,
                "willSaveWaitUntil": false,
                "didSave": true,
            },
            "publishDiagnostics": {
                "relatedInformation": false,
                "versionSupport": true,
                "tagSupport": { "valueSet": [1, 2] },
                "codeDescriptionSupport": true,
                "dataSupport": false,
            },
            "completion": {
                "dynamicRegistration": false,
                "contextSupport": true,
                "completionItem": {
                    "snippetSupport": true,
                    "commitCharactersSupport": false,
                    "documentationFormat": ["markdown", "plaintext"],
                    "deprecatedSupport": true,
                    "preselectSupport": true,
                    "tagSupport": { "valueSet": [1] },
                    // Enter inserts (up to the cursor), Tab replaces the whole word.
                    "insertReplaceSupport": true,
                    // Additional edits (imports) may come with the resolved item: flux-app applies
                    // them when it arrives. rust-analyzer offers auto-import items only then.
                    "resolveSupport": {
                        "properties": ["documentation", "detail", "additionalTextEdits"]
                    },
                    // Multi-line completions get the line's indentation (`adjustIndentation`).
                    "insertTextModeSupport": { "valueSet": [1, 2] },
                    "labelDetailsSupport": true,
                },
                "completionItemKind": { "valueSet": (1..=25).collect::<Vec<u32>>() },
            },
            "hover": {
                "dynamicRegistration": false,
                "contentFormat": ["markdown", "plaintext"],
            },
            "signatureHelp": {
                "dynamicRegistration": false,
                "signatureInformation": {
                    "documentationFormat": ["markdown", "plaintext"],
                    "parameterInformation": { "labelOffsetSupport": true },
                    "activeParameterSupport": true,
                },
            },
            "definition": { "dynamicRegistration": false, "linkSupport": true },
            "declaration": { "dynamicRegistration": false, "linkSupport": true },
            "typeDefinition": { "dynamicRegistration": false, "linkSupport": true },
            "implementation": { "dynamicRegistration": false, "linkSupport": true },
            "references": { "dynamicRegistration": false },
            "formatting": { "dynamicRegistration": false },
            "rangeFormatting": { "dynamicRegistration": false },
            "rename": {
                "dynamicRegistration": false,
                "prepareSupport": true,
                "prepareSupportDefaultBehavior": 1,
                "honorsChangeAnnotations": false,
            },
        },
        "window": {
            "workDoneProgress": true,
            "showMessage": { "messageActionItem": { "additionalPropertiesSupport": false } },
            "showDocument": { "support": false },
        },
    })
}

/// A message for the log: the first few hundred characters.
fn abbreviate(body: &[u8]) -> String {
    truncate(&String::from_utf8_lossy(body), 600)
}

fn truncate(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_events_from_work_done_progress() {
        let event = |value: Value| {
            let params: ProgressParams = serde_json::from_value(value).unwrap();
            let ProgressParamsValue::WorkDone(progress) = params.value;
            progress_event(params.token, progress)
        };
        let begin = event(json!({
            "token": "rustAnalyzer/Indexing",
            "value": { "kind": "begin", "title": "Indexing", "percentage": 0 },
        }));
        assert!(matches!(
            begin,
            ServerEvent::Progress { ref token, title: Some(ref title), percentage: Some(0), done: false, .. }
                if token == "rustAnalyzer/Indexing" && title == "Indexing"
        ));
        let report = event(json!({
            "token": 7,
            "value": { "kind": "report", "message": "3/10", "percentage": 30 },
        }));
        assert!(matches!(
            report,
            ServerEvent::Progress { ref token, title: None, message: Some(_), percentage: Some(30), done: false }
                if token == "7"
        ));
        let end = event(json!({ "token": 7, "value": { "kind": "end" } }));
        assert!(matches!(end, ServerEvent::Progress { done: true, .. }));
    }

    #[test]
    fn outdated_errors() {
        let error = |code| RequestError::Server {
            code,
            message: String::new(),
        };
        assert!(error(-32801).is_outdated());
        assert!(error(-32800).is_outdated());
        assert!(!error(-32603).is_outdated());
        assert!(!RequestError::Exited.is_outdated());
    }

    #[test]
    fn long_text_is_truncated_by_characters() {
        assert_eq!(truncate("ЯЯЯЯ", 2), "ЯЯ…");
        assert_eq!(truncate("ab", 2), "ab");
    }
}
