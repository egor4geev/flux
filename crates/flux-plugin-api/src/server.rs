//! The plugin's server on this Mac — on 127.0.0.1 only, as the manifest's `server` permission
//! allows: the redirect of a web service's sign-in (OAuth), programs that talk to the IDE over
//! HTTP or WebSocket. Requests come as `Event::ServerRequest`, WebSocket connections and messages
//! as `Event::WsOpened`, `Event::WsMessage`, `Event::WsClosed`:
//!
//! ```ignore
//! let port = server::listen()?;
//! system::open_url(&server::url(port, "/"))?;
//! // …
//! Event::ServerRequest(request) => match request.route() {
//!     "/" => server::respond_html(request.id, 200, "<h1>Hello</h1>"),
//!     "/callback" => self.signed_in(request.query("code")),
//!     _ => server::not_found(request.id),
//! },
//! Event::WsMessage((connection, WsMessage::Text(text))) => {
//!     server::ws_send_text(connection, &text)?;
//! }
//! ```
//!
//! Flux answers 403 itself to a request whose `Host` isn't the server's own, and 504 to one the
//! plugin doesn't answer within 60 s.

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::host::server as raw;
pub use crate::host::server::{ServerRequest, ServerResponse, WsMessage, WsOpen};

/// Starts the plugin's server on a free port and returns the port; a running server is stopped
/// first.
pub fn listen() -> Result<u16, String> {
    raw::listen(None)
}

/// Starts the server on `port`: for a web service whose sign-in redirects to a fixed address.
pub fn listen_on(port: u16) -> Result<u16, String> {
    raw::listen(Some(port))
}

/// Stops the server and closes its connections.
pub fn stop() {
    raw::stop()
}

/// The server's address for `path`: `http://127.0.0.1:<port>/callback`.
pub fn url(port: u16, path: &str) -> String {
    let slash = if path.starts_with('/') { "" } else { "/" };
    format!("http://127.0.0.1:{port}{slash}{path}")
}

/// Answers a request with a status, a content type and a body.
pub fn respond(request: u64, status: u16, content_type: &str, body: impl Into<Vec<u8>>) {
    raw::respond(
        request,
        &ServerResponse {
            status,
            headers: vec![("Content-Type".to_string(), content_type.to_string())],
            body: body.into(),
        },
    )
}

/// Answers with plain text.
pub fn respond_text(request: u64, status: u16, text: &str) {
    respond(request, status, "text/plain; charset=utf-8", text)
}

/// Answers with a page: what the browser shows after a sign-in.
pub fn respond_html(request: u64, status: u16, html: &str) {
    respond(request, status, "text/html; charset=utf-8", html)
}

/// Answers with JSON.
pub fn respond_json<T: Serialize + ?Sized>(request: u64, status: u16, value: &T) {
    match serde_json::to_vec(value) {
        Ok(body) => respond(request, status, "application/json", body),
        Err(err) => respond_text(request, 500, &format!("The answer doesn't serialize: {err}")),
    }
}

/// Answers 404.
pub fn not_found(request: u64) {
    respond_text(request, 404, "Not found")
}

/// Answers with a stream of server-sent events: then [`send_event`] for each, [`end_events`] to
/// finish. A client that goes away comes as `Event::StreamClosed(request)`.
pub fn start_events(request: u64) {
    raw::respond_stream(
        request,
        200,
        &[
            ("Content-Type".to_string(), "text/event-stream".to_string()),
            ("Cache-Control".to_string(), "no-cache".to_string()),
        ],
    )
}

/// Sends one server-sent event of a stream; `event` — its type (none — a plain message).
pub fn send_event(request: u64, event: Option<&str>, data: &str) -> Result<(), String> {
    raw::send_chunk(request, sse_frame(event, data).as_bytes())
}

/// Ends a stream of events.
pub fn end_events(request: u64) {
    raw::end_stream(request)
}

/// Sends a text message over a WebSocket connection.
pub fn ws_send_text(connection: u64, text: &str) -> Result<(), String> {
    raw::ws_send(connection, &WsMessage::Text(text.to_string()))
}

/// Sends a binary message over a WebSocket connection.
pub fn ws_send_binary(connection: u64, bytes: &[u8]) -> Result<(), String> {
    raw::ws_send(connection, &WsMessage::Binary(bytes.to_vec()))
}

/// Closes a WebSocket connection.
pub fn ws_close(connection: u64) {
    raw::ws_close(connection)
}

/// An event as `text/event-stream` writes it: `event:`, a `data:` per line, an empty line.
fn sse_frame(event: Option<&str>, data: &str) -> String {
    let mut frame = String::new();
    if let Some(event) = event {
        frame.push_str(&format!("event: {event}\n"));
    }
    for line in data.split('\n') {
        frame.push_str(&format!("data: {line}\n"));
    }
    frame.push('\n');
    frame
}

/// The path without its query: `/callback?code=x` → `/callback`.
fn route(path: &str) -> &str {
    path.split_once('?').map_or(path, |(route, _)| route)
}

fn query(path: &str) -> &str {
    path.split_once('?').map_or("", |(_, query)| query)
}

impl ServerRequest {
    /// The path without its query: what to route on.
    pub fn route(&self) -> &str {
        route(&self.path)
    }

    /// A parameter of the query, decoded: `request.query("code")`.
    pub fn query(&self, name: &str) -> Option<String> {
        crate::url::find(query(&self.path), name)
    }

    /// The first header of that name (names compare without case).
    pub fn header(&self, name: &str) -> Option<&str> {
        crate::http::header(&self.headers, name)
    }

    /// The body as text (invalid UTF-8 replaced).
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The body as JSON.
    pub fn json<T: DeserializeOwned>(&self) -> Result<T, String> {
        serde_json::from_slice(&self.body).map_err(|err| format!("Not the JSON expected: {err}"))
    }

    /// A field of a form's body (`application/x-www-form-urlencoded`), decoded.
    pub fn form(&self, name: &str) -> Option<String> {
        crate::url::find(&self.text(), name)
    }
}

impl WsOpen {
    /// The path without its query.
    pub fn route(&self) -> &str {
        route(&self.path)
    }

    /// A parameter of the query, decoded.
    pub fn query(&self, name: &str) -> Option<String> {
        crate::url::find(query(&self.path), name)
    }

    /// The first header of that name: a token the client was given, say.
    pub fn header(&self, name: &str) -> Option<&str> {
        crate::http::header(&self.headers, name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_events() {
        assert_eq!(sse_frame(None, "hi"), "data: hi\n\n");
        assert_eq!(
            sse_frame(Some("tick"), "one\ntwo"),
            "event: tick\ndata: one\ndata: two\n\n"
        );
        // What the frames say is what the reader of the stream gets back.
        let mut stream = crate::http::EventStream::new();
        let events = stream.feed(sse_frame(Some("tick"), "one\ntwo").as_bytes());
        assert_eq!(events[0].data, "one\ntwo");
    }

    #[test]
    fn reads_requests() {
        let request = ServerRequest {
            id: 1,
            method: "GET".into(),
            path: "/callback?code=a%2Fb&state=xyz".into(),
            headers: vec![("Host".into(), "127.0.0.1:8123".into())],
            body: b"name=Flux+IDE".to_vec(),
        };
        assert_eq!(request.route(), "/callback");
        assert_eq!(request.query("code").as_deref(), Some("a/b"));
        assert_eq!(request.query("nope"), None);
        assert_eq!(request.header("host"), Some("127.0.0.1:8123"));
        assert_eq!(request.form("name").as_deref(), Some("Flux IDE"));
        assert_eq!(url(8123, "callback"), "http://127.0.0.1:8123/callback");
        assert_eq!(url(8123, "/"), "http://127.0.0.1:8123/");
    }
}
