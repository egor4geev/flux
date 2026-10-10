//! `server`: the plugin's server on 127.0.0.1, as the manifest's `server` permission allows —
//! HTTP (whole answers and streams) and WebSocket. Its threads post `server-request`,
//! `stream-closed`, `ws-opened`, `ws-message` and `ws-closed` events into the plugin's queue; a
//! request whose `Host` isn't the server's own gets 403 without the plugin, one without an answer
//! for [`ANSWER_TIMEOUT`] gets 504. The server stops when the plugin does ([`Server`]).
//!
//! A thread accepts connections, and each connection has a thread of its own: it reads a request
//! (keep-alive: then the next one), hands it to the plugin and waits for the answer — whole, or a
//! stream of chunks (server-sent events) the plugin sends while the connection's thread watches
//! the client. A WebSocket upgrade is answered here (101 with the accept key), then the
//! connection's thread reads frames in short slices and writes the plugin's messages between them.

use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tungstenite::protocol::{Role, WebSocket};
use tungstenite::{Bytes, Message};

use super::window::missing;
use crate::api::bindings::flux::plugin::server;
use crate::api::events::Event;
use crate::host::LogSignal;
use crate::log::{Level, PluginLog};
use crate::runtime::{EventSender, State};

/// A request the plugin doesn't answer in this time gets 504.
const ANSWER_TIMEOUT: Duration = if cfg!(test) {
    Duration::from_secs(2)
} else {
    Duration::from_secs(60)
};
/// The largest head of a request (the request line and the headers).
const MAX_HEAD: usize = 64 << 10;
/// The largest body of a request.
const MAX_BODY: usize = 16 << 20;
/// A connection without a request for this long is closed.
const IDLE: Duration = Duration::from_secs(30);
/// Writing to a client that doesn't read gives up after this long.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// How often the threads look around: the accept loop for the stop, a stream for its client, a
/// WebSocket for the plugin's messages.
const TICK: Duration = Duration::from_millis(25);
/// How long a WebSocket closed by the plugin waits for the client's close.
const CLOSE_WAIT: Duration = Duration::from_secs(2);

/// The plugin's server, if it listens: stopped when the plugin stops (dropped with its store).
#[derive(Default)]
pub(crate) struct Server {
    running: Option<Arc<Shared>>,
}

impl Server {
    /// Stops the server, if it runs: true if it did.
    fn stop(&mut self) -> bool {
        match self.running.take() {
            Some(shared) => {
                shared.shut_down();
                true
            }
            None => false,
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

/// What the server's threads share with the plugin's calls.
struct Shared {
    port: u16,
    /// Taken (closed) when the server stops: a new connection is refused at once.
    listener: Mutex<Option<TcpListener>>,
    stop: AtomicBool,
    /// Ids of requests and connections: one sequence, so that they never mix up.
    next_id: AtomicU64,
    events: EventSender,
    log: PluginLog,
    signal: Arc<LogSignal>,
    /// Requests waiting for the plugin's answer.
    pending: Mutex<HashMap<u64, Sender<Answer>>>,
    /// Streamed answers in progress.
    streams: Mutex<HashMap<u64, Sender<Part>>>,
    /// Open WebSocket connections.
    sockets: Mutex<HashMap<u64, Sender<Outgoing>>>,
    /// Every open connection: shut down when the server stops.
    connections: Mutex<HashMap<u64, TcpStream>>,
}

/// The plugin's answer to a request.
enum Answer {
    Whole(server::ServerResponse),
    Stream {
        status: u16,
        headers: Vec<(String, String)>,
        parts: Receiver<Part>,
    },
}

/// A piece of a streamed answer.
enum Part {
    Chunk(Vec<u8>),
    End,
}

/// What the plugin sends to a WebSocket connection.
enum Outgoing {
    Message(server::WsMessage),
    Close,
}

impl Shared {
    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn log(&self, level: Level, text: &str) {
        self.log.write(level, text);
        self.signal.logged();
    }

    /// Stops accepting, closes every connection; the plugin's answers in flight go nowhere.
    fn shut_down(&self) {
        self.stop.store(true, Ordering::Relaxed);
        self.listener.lock().unwrap().take();
        for stream in self.connections.lock().unwrap().values() {
            let _ = stream.shutdown(Shutdown::Both);
        }
        self.pending.lock().unwrap().clear();
        self.streams.lock().unwrap().clear();
        self.sockets.lock().unwrap().clear();
        self.log(
            Level::Info,
            &format!("Server on 127.0.0.1:{} stopped", self.port),
        );
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

impl server::Host for State {
    fn listen(&mut self, port: Option<u16>) -> Result<u16, String> {
        let host = &mut self.host;
        if !host.entry.manifest.permissions.server {
            return Err(missing("run a server", "server = true"));
        }
        host.server.stop();
        let wanted = port.unwrap_or(0);
        let listener = TcpListener::bind(("127.0.0.1", wanted))
            .map_err(|err| format!("Couldn't listen on 127.0.0.1:{wanted}: {err}"))?;
        let port = listener
            .local_addr()
            .map_err(|err| format!("Couldn't listen on 127.0.0.1:{wanted}: {err}"))?
            .port();
        listener
            .set_nonblocking(true)
            .map_err(|err| format!("Couldn't listen on 127.0.0.1:{port}: {err}"))?;
        let shared = Arc::new(Shared {
            port,
            listener: Mutex::new(Some(listener)),
            stop: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
            events: host.events.clone(),
            log: host.log.clone(),
            signal: host.signal.clone(),
            pending: Mutex::default(),
            streams: Mutex::default(),
            sockets: Mutex::default(),
            connections: Mutex::default(),
        });
        std::thread::Builder::new()
            .name(format!("plugin {} server", host.id))
            .spawn({
                let shared = shared.clone();
                move || accept(shared)
            })
            .map_err(|err| format!("Couldn't start the server's thread: {err}"))?;
        shared.log(Level::Info, &format!("Server listens on 127.0.0.1:{port}"));
        host.server.running = Some(shared);
        Ok(port)
    }

    fn stop(&mut self) {
        self.host.server.stop();
    }

    fn respond(&mut self, request: u64, response: server::ServerResponse) {
        if let Some(answer) = self.pending(request, "server.respond") {
            let _ = answer.send(Answer::Whole(response));
        }
    }

    fn respond_stream(&mut self, request: u64, status: u16, headers: Vec<(String, String)>) {
        let Some(answer) = self.pending(request, "server.respond-stream") else {
            return;
        };
        let Some(shared) = &self.host.server.running else {
            return;
        };
        // The stream is known before the connection's thread hears of it: `send-chunk` right after
        // this call finds it.
        let (parts, receiver) = mpsc::channel();
        shared.streams.lock().unwrap().insert(request, parts);
        let _ = answer.send(Answer::Stream {
            status,
            headers,
            parts: receiver,
        });
    }

    fn send_chunk(&mut self, request: u64, chunk: Vec<u8>) -> Result<(), String> {
        let closed = || format!("The stream of the request {request} is closed");
        let shared = self.host.server.running.as_ref().ok_or_else(not_running)?;
        let streams = shared.streams.lock().unwrap();
        let parts = streams.get(&request).ok_or_else(closed)?;
        parts.send(Part::Chunk(chunk)).map_err(|_| closed())
    }

    fn end_stream(&mut self, request: u64) {
        let Some(shared) = &self.host.server.running else {
            return;
        };
        if let Some(parts) = shared.streams.lock().unwrap().remove(&request) {
            let _ = parts.send(Part::End);
        }
    }

    fn ws_send(&mut self, connection: u64, message: server::WsMessage) -> Result<(), String> {
        let closed = || format!("The WebSocket connection {connection} is closed");
        let shared = self.host.server.running.as_ref().ok_or_else(not_running)?;
        let sockets = shared.sockets.lock().unwrap();
        let socket = sockets.get(&connection).ok_or_else(closed)?;
        socket
            .send(Outgoing::Message(message))
            .map_err(|_| closed())
    }

    fn ws_close(&mut self, connection: u64) {
        let Some(shared) = &self.host.server.running else {
            return;
        };
        if let Some(socket) = shared.sockets.lock().unwrap().get(&connection) {
            let _ = socket.send(Outgoing::Close);
        }
    }
}

impl State {
    /// The request waiting for the plugin's answer; a warning in the log when there is none (the
    /// author looks there).
    fn pending(&mut self, request: u64, what: &str) -> Option<Sender<Answer>> {
        let answer = self
            .host
            .server
            .running
            .as_ref()
            .and_then(|shared| shared.pending.lock().unwrap().remove(&request));
        if answer.is_none() {
            self.host.warn(&format!(
                "{what}: no request {request} waits for an answer (answered already, waited \
                 longer than {} s, or the server isn't running)",
                ANSWER_TIMEOUT.as_secs()
            ));
        }
        answer
    }
}

fn not_running() -> String {
    "The plugin's server isn't running".into()
}

/// The accepting thread: a thread per connection, until the server stops.
fn accept(shared: Arc<Shared>) {
    while !shared.stopped() {
        let accepted = match &*shared.listener.lock().unwrap() {
            Some(listener) => listener.accept(),
            None => return,
        };
        match accepted {
            Ok((stream, _)) => {
                let spawned = std::thread::Builder::new()
                    .name("plugin server connection".into())
                    .spawn({
                        let shared = shared.clone();
                        move || serve(stream, shared)
                    });
                if let Err(err) = spawned {
                    shared.log(
                        Level::Warn,
                        &format!("Server: couldn't start a thread: {err}"),
                    );
                }
            }
            Err(err) if err.kind() == ErrorKind::WouldBlock => std::thread::sleep(TICK),
            Err(err) => {
                shared.log(Level::Warn, &format!("Server: couldn't accept: {err}"));
                std::thread::sleep(TICK);
            }
        }
    }
}

/// A connection's thread.
fn serve(stream: TcpStream, shared: Arc<Shared>) {
    // An accepted socket keeps the listener's non-blocking mode on macOS.
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_nodelay(true);
    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
    let id = shared.next_id();
    if let Ok(clone) = stream.try_clone() {
        shared.connections.lock().unwrap().insert(id, clone);
    }
    // Stopped while the connection was being accepted: the stop didn't see it.
    if !shared.stopped() {
        Connection {
            stream,
            buffer: Vec::new(),
            shared: shared.clone(),
        }
        .serve();
    }
    shared.connections.lock().unwrap().remove(&id);
}

/// A request's head.
struct Head {
    method: String,
    /// The path with the query.
    target: String,
    /// HTTP/1.1 (true) or 1.0.
    http11: bool,
    /// Names in lowercase.
    headers: Vec<(String, String)>,
}

impl Head {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header == name)
            .map(|(_, value)| value.trim())
    }

    /// Whether a comma-separated header lists `token` (`Connection: keep-alive, Upgrade`).
    fn lists(&self, name: &str, token: &str) -> bool {
        self.headers
            .iter()
            .filter(|(header, _)| header == name)
            .flat_map(|(_, value)| value.split(','))
            .any(|item| item.trim().eq_ignore_ascii_case(token))
    }

    fn keep_alive(&self) -> bool {
        if self.http11 {
            !self.lists("connection", "close")
        } else {
            self.lists("connection", "keep-alive")
        }
    }
}

/// Why a request isn't served: the status of the short answer it gets.
struct Refusal(u16);

struct Connection {
    stream: TcpStream,
    /// Bytes read and not used yet: the rest of a head, a body, the next request.
    buffer: Vec<u8>,
    shared: Arc<Shared>,
}

impl Connection {
    fn serve(mut self) {
        loop {
            let head = match self.read_head() {
                Ok(Some(head)) => head,
                Ok(None) => return,
                Err(Refusal(status)) => return self.refuse(status),
            };
            let port = self.shared.port;
            let host = head.header("host").unwrap_or_default().to_ascii_lowercase();
            if host != format!("127.0.0.1:{port}") && host != format!("localhost:{port}") {
                self.shared.log(
                    Level::Warn,
                    &format!(
                        "Server: {} {} refused: Host \"{host}\" isn't the server's own",
                        head.method,
                        path_of(&head.target)
                    ),
                );
                return self.refuse(403);
            }
            if head.method == "GET" && head.lists("upgrade", "websocket") {
                return self.websocket(head);
            }
            let body = match self.read_body(&head) {
                Ok(body) => body,
                Err(Refusal(status)) => return self.refuse(status),
            };
            let started = Instant::now();
            let id = self.shared.next_id();
            let (answer, receiver) = mpsc::channel();
            self.shared.pending.lock().unwrap().insert(id, answer);
            let request = server::ServerRequest {
                id,
                method: head.method.clone(),
                path: head.target.clone(),
                headers: head.headers.clone(),
                body,
            };
            if !self.shared.events.send(Event::ServerRequest(request)) {
                self.shared.pending.lock().unwrap().remove(&id);
                return self.refuse(503);
            }
            let answer = receiver.recv_timeout(ANSWER_TIMEOUT);
            self.shared.pending.lock().unwrap().remove(&id);
            let line = format!("Server: {} {}", head.method, path_of(&head.target));
            match answer {
                Ok(Answer::Whole(response)) => {
                    self.shared.log(
                        Level::Debug,
                        &format!(
                            "{line} → {} · {} ms",
                            response.status,
                            started.elapsed().as_millis()
                        ),
                    );
                    let keep_alive = head.keep_alive() && !self.shared.stopped();
                    let body = if head.method == "HEAD" {
                        &[][..]
                    } else {
                        &response.body[..]
                    };
                    let written = self.write_head(
                        response.status,
                        &response.headers,
                        &[
                            ("Content-Length", &response.body.len().to_string()),
                            (
                                "Connection",
                                if keep_alive { "keep-alive" } else { "close" },
                            ),
                        ],
                    );
                    if written
                        .and_then(|()| self.stream.write_all(body))
                        .and_then(|()| self.stream.flush())
                        .is_err()
                        || !keep_alive
                    {
                        return;
                    }
                }
                Ok(Answer::Stream {
                    status,
                    headers,
                    parts,
                }) => {
                    self.shared
                        .log(Level::Debug, &format!("{line} → {status} · stream"));
                    return self.stream(id, status, &headers, parts);
                }
                Err(RecvTimeoutError::Timeout) => {
                    self.shared.log(
                        Level::Warn,
                        &format!("{line}: no answer in {} s → 504", ANSWER_TIMEOUT.as_secs()),
                    );
                    return self.refuse(504);
                }
                // The server stopped, or the plugin did.
                Err(RecvTimeoutError::Disconnected) => return self.refuse(503),
            }
        }
    }

    /// Reads the next request's head; none when the client closed the connection (or stayed
    /// silent too long) between requests.
    fn read_head(&mut self) -> Result<Option<Head>, Refusal> {
        let _ = self.stream.set_read_timeout(Some(IDLE));
        loop {
            if !self.buffer.is_empty() {
                let mut headers = [httparse::EMPTY_HEADER; 100];
                let mut request = httparse::Request::new(&mut headers);
                match request.parse(&self.buffer) {
                    Ok(httparse::Status::Complete(length)) => {
                        let head = Head {
                            method: request.method.unwrap_or_default().to_string(),
                            target: request.path.unwrap_or_default().to_string(),
                            http11: request.version == Some(1),
                            headers: request
                                .headers
                                .iter()
                                .map(|header| {
                                    (
                                        header.name.to_ascii_lowercase(),
                                        String::from_utf8_lossy(header.value).into_owned(),
                                    )
                                })
                                .collect(),
                        };
                        self.buffer.drain(..length);
                        return Ok(Some(head));
                    }
                    Ok(httparse::Status::Partial) if self.buffer.len() > MAX_HEAD => {
                        return Err(Refusal(431));
                    }
                    Ok(httparse::Status::Partial) => {}
                    Err(httparse::Error::TooManyHeaders) => return Err(Refusal(431)),
                    Err(_) => return Err(Refusal(400)),
                }
            }
            match self.fill() {
                Ok(0) if self.buffer.is_empty() => return Ok(None),
                Ok(0) => return Err(Refusal(400)),
                Ok(_) => {}
                Err(_) if self.buffer.is_empty() => return Ok(None),
                Err(_) => return Err(Refusal(408)),
            }
        }
    }

    /// Reads more bytes into the buffer: how many (0 — the client closed the connection).
    fn fill(&mut self) -> std::io::Result<usize> {
        let mut chunk = [0; 16 << 10];
        let read = self.stream.read(&mut chunk)?;
        self.buffer.extend_from_slice(&chunk[..read]);
        Ok(read)
    }

    /// The body: `Content-Length` bytes, or `chunked` ones; nothing without either.
    fn read_body(&mut self, head: &Head) -> Result<Vec<u8>, Refusal> {
        let chunked = head.lists("transfer-encoding", "chunked");
        let length = match head.header("content-length") {
            Some(length) if !chunked => Some(length.parse::<usize>().map_err(|_| Refusal(400))?),
            _ => None,
        };
        if length.is_some_and(|length| length > MAX_BODY) {
            return Err(Refusal(413));
        }
        if (chunked || length.is_some_and(|length| length > 0))
            && head.lists("expect", "100-continue")
        {
            let _ = self.stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n");
        }
        if chunked {
            return self.read_chunked();
        }
        let Some(length) = length else {
            return Ok(Vec::new());
        };
        while self.buffer.len() < length {
            match self.fill() {
                Ok(0) | Err(_) => return Err(Refusal(400)),
                Ok(_) => {}
            }
        }
        Ok(self.buffer.drain(..length).collect())
    }

    fn read_chunked(&mut self) -> Result<Vec<u8>, Refusal> {
        let mut body = Vec::new();
        loop {
            let line = self.read_line()?;
            let size = line.split(';').next().unwrap_or_default().trim();
            let size = usize::from_str_radix(size, 16).map_err(|_| Refusal(400))?;
            if body.len() + size > MAX_BODY {
                return Err(Refusal(413));
            }
            if size == 0 {
                // Trailers, then the empty line.
                while !self.read_line()?.is_empty() {}
                return Ok(body);
            }
            while self.buffer.len() < size + 2 {
                match self.fill() {
                    Ok(0) | Err(_) => return Err(Refusal(400)),
                    Ok(_) => {}
                }
            }
            body.extend(self.buffer.drain(..size));
            self.buffer.drain(..2);
        }
    }

    /// A line of a chunked body, without its CRLF.
    fn read_line(&mut self) -> Result<String, Refusal> {
        loop {
            if let Some(end) = self.buffer.windows(2).position(|pair| pair == b"\r\n") {
                let line = String::from_utf8_lossy(&self.buffer[..end]).into_owned();
                self.buffer.drain(..end + 2);
                return Ok(line);
            }
            if self.buffer.len() > MAX_HEAD {
                return Err(Refusal(400));
            }
            match self.fill() {
                Ok(0) | Err(_) => return Err(Refusal(400)),
                Ok(_) => {}
            }
        }
    }

    /// The status line and the headers: the plugin's (but those of the framing, which are
    /// Flux's), then `own`.
    fn write_head(
        &mut self,
        status: u16,
        headers: &[(String, String)],
        own: &[(&str, &str)],
    ) -> std::io::Result<()> {
        let status = if (100..=999).contains(&status) {
            status
        } else {
            500
        };
        let reason = ureq::http::StatusCode::from_u16(status)
            .ok()
            .and_then(|code| code.canonical_reason())
            .unwrap_or("");
        let mut head = format!("HTTP/1.1 {status} {reason}\r\n");
        for (name, value) in headers {
            let framing = ["content-length", "transfer-encoding", "connection"]
                .iter()
                .any(|own| name.eq_ignore_ascii_case(own));
            let broken =
                name.is_empty() || name.contains(['\r', '\n', ':']) || value.contains(['\r', '\n']);
            if !framing && !broken {
                head.push_str(&format!("{name}: {value}\r\n"));
            }
        }
        for (name, value) in own {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str("\r\n");
        self.stream.write_all(head.as_bytes())
    }

    /// A short answer of the server's own, and the end of the connection.
    fn refuse(&mut self, status: u16) {
        let reason = ureq::http::StatusCode::from_u16(status)
            .ok()
            .and_then(|code| code.canonical_reason())
            .unwrap_or("Error");
        let body = format!("{status} {reason}\n");
        let _ = self.write_head(
            status,
            &[("Content-Type".into(), "text/plain; charset=utf-8".into())],
            &[
                ("Content-Length", &body.len().to_string()),
                ("Connection", "close"),
            ],
        );
        let _ = self.stream.write_all(body.as_bytes());
        let _ = self.stream.flush();
    }

    /// A streamed answer: chunks as the plugin sends them, until it ends the stream, the client
    /// goes away (`stream-closed`) or the server stops.
    fn stream(
        &mut self,
        id: u64,
        status: u16,
        headers: &[(String, String)],
        parts: Receiver<Part>,
    ) {
        let written = self.write_head(
            status,
            headers,
            &[("Transfer-Encoding", "chunked"), ("Connection", "close")],
        );
        let mut gone = written.and_then(|()| self.stream.flush()).is_err();
        while !gone {
            match parts.recv_timeout(TICK) {
                Ok(Part::Chunk(chunk)) if chunk.is_empty() => {}
                Ok(Part::Chunk(chunk)) => {
                    let frame = format!("{:x}\r\n", chunk.len());
                    gone = self
                        .stream
                        .write_all(frame.as_bytes())
                        .and_then(|()| self.stream.write_all(&chunk))
                        .and_then(|()| self.stream.write_all(b"\r\n"))
                        .and_then(|()| self.stream.flush())
                        .is_err();
                }
                Ok(Part::End) | Err(RecvTimeoutError::Disconnected) => {
                    let _ = self.stream.write_all(b"0\r\n\r\n");
                    let _ = self.stream.flush();
                    return;
                }
                Err(RecvTimeoutError::Timeout) if self.shared.stopped() => return,
                Err(RecvTimeoutError::Timeout) => gone = self.client_gone(),
            }
        }
        // The client went away while the plugin still streamed.
        if self.shared.streams.lock().unwrap().remove(&id).is_some() {
            self.shared.events.send(Event::StreamClosed(id));
        }
    }

    /// Whether the client closed the connection: a read that wouldn't block finds its end.
    fn client_gone(&mut self) -> bool {
        if self.stream.set_nonblocking(true).is_err() {
            return true;
        }
        let mut byte = [0];
        let gone = match self.stream.peek(&mut byte) {
            Ok(0) => true,
            Ok(_) => false,
            Err(err) => err.kind() != ErrorKind::WouldBlock,
        };
        let _ = self.stream.set_nonblocking(false);
        gone
    }

    /// Answers the upgrade, then serves the WebSocket connection until either side closes it.
    fn websocket(mut self, head: Head) {
        let key = head.header("sec-websocket-key").map(str::to_string);
        let (Some(key), Some("13")) = (key, head.header("sec-websocket-version")) else {
            let _ = self.write_head(
                426,
                &[("Sec-WebSocket-Version".into(), "13".into())],
                &[("Content-Length", "0"), ("Connection", "close")],
            );
            return;
        };
        let accept = tungstenite::handshake::derive_accept_key(key.as_bytes());
        let answer = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Accept: {accept}\r\n\r\n"
        );
        if self
            .stream
            .write_all(answer.as_bytes())
            .and_then(|()| self.stream.flush())
            .is_err()
        {
            return;
        }
        let _ = self.stream.set_read_timeout(Some(TICK));
        let shared = self.shared.clone();
        let leftover = std::mem::take(&mut self.buffer);
        let mut socket = WebSocket::from_partially_read(self.stream, leftover, Role::Server, None);
        let connection = shared.next_id();
        let (outgoing, messages) = mpsc::channel();
        shared.sockets.lock().unwrap().insert(connection, outgoing);
        shared.log(
            Level::Debug,
            &format!("Server: WebSocket {} opened", path_of(&head.target)),
        );
        shared.events.send(Event::WsOpened(server::WsOpen {
            connection,
            path: head.target,
            headers: head.headers,
        }));
        serve_websocket(&mut socket, connection, &messages, &shared);
        shared.sockets.lock().unwrap().remove(&connection);
        shared.events.send(Event::WsClosed(connection));
    }
}

/// Reads the client's frames in slices of [`TICK`], and between them writes the plugin's
/// messages; returns when the connection is over.
fn serve_websocket(
    socket: &mut WebSocket<TcpStream>,
    connection: u64,
    messages: &Receiver<Outgoing>,
    shared: &Shared,
) {
    let mut closing: Option<Instant> = None;
    loop {
        loop {
            match messages.try_recv() {
                Ok(Outgoing::Message(message)) => {
                    let message = match message {
                        server::WsMessage::Text(text) => Message::text(text),
                        server::WsMessage::Binary(bytes) => Message::Binary(Bytes::from(bytes)),
                    };
                    if socket.send(message).is_err() {
                        return;
                    }
                }
                Ok(Outgoing::Close) if closing.is_none() => {
                    closing = Some(Instant::now());
                    let _ = socket.close(None);
                    let _ = socket.flush();
                }
                Ok(Outgoing::Close) => {}
                Err(TryRecvError::Empty) => break,
                // The server stopped.
                Err(TryRecvError::Disconnected) => return,
            }
        }
        if shared.stopped() || closing.is_some_and(|since| since.elapsed() > CLOSE_WAIT) {
            return;
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                let text = text.as_str().to_string();
                shared.events.send(Event::WsMessage((
                    connection,
                    server::WsMessage::Text(text),
                )));
            }
            Ok(Message::Binary(bytes)) => {
                shared.events.send(Event::WsMessage((
                    connection,
                    server::WsMessage::Binary(bytes.to_vec()),
                )));
            }
            // Pings are answered by tungstenite itself; a close is answered on the next write.
            Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => {}
            Ok(Message::Close(_)) => {
                let _ = socket.flush();
            }
            Err(tungstenite::Error::Io(err))
                if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => return,
        }
    }
}

/// The path of a request target without its query (they carry codes and tokens), for the log.
fn path_of(target: &str) -> &str {
    target.split(['?', '#']).next().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_in_the_log_have_no_query() {
        assert_eq!(path_of("/callback?code=secret"), "/callback");
        assert_eq!(path_of("/"), "/");
    }

    #[test]
    fn headers_lists() {
        let head = Head {
            method: "GET".into(),
            target: "/".into(),
            http11: true,
            headers: vec![
                ("connection".into(), "keep-alive, Upgrade".into()),
                ("upgrade".into(), "websocket".into()),
            ],
        };
        assert!(head.lists("connection", "upgrade"));
        assert!(head.lists("upgrade", "WebSocket"));
        assert!(head.keep_alive());
        assert_eq!(head.header("upgrade"), Some("websocket"));
    }
}
