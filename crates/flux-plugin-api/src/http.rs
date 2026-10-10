//! Requests to web services, as the manifest's `network` permission allows (each request and each
//! redirect goes only to the hosts it lists). Build a request, then wait for the whole response
//! ([`Request::fetch`]) or start it in the background ([`Request::start`]) and take the response
//! as events:
//!
//! ```ignore
//! let repo = http::get("https://api.github.com/repos/egor4geev/flux")
//!     .header("Accept", "application/vnd.github+json")
//!     .fetch()?;
//! let stars = repo.json::<serde_json::Value>()?["stargazers_count"].clone();
//!
//! // In the background: Event::HttpResponse(head), Event::HttpBody((id, chunk))…, then
//! // Event::HttpDone((id, result)).
//! self.download = Some(http::get(url).start());
//! ```
//!
//! Waiting in `fetch` doesn't count toward the plugin's time limit, but the plugin answers nothing
//! else meanwhile: a long download, a stream of server-sent events ([`EventStream`]) go with
//! `start`.

use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::host::http as raw;
pub use crate::host::http::{HttpError, Response, ResponseHead};

/// A request being built: [`get`], [`post`]…, the methods add to it.
#[derive(Debug, Clone)]
pub struct Request(raw::Request);

/// A request with any method: `"PROPFIND"`, `"REPORT"` (CalDAV)…
pub fn request(method: &str, url: &str) -> Request {
    Request(raw::Request {
        method: method.to_string(),
        url: url.to_string(),
        headers: Vec::new(),
        body: None,
        timeout_ms: None,
        follow_redirects: true,
    })
}

pub fn get(url: &str) -> Request {
    request("GET", url)
}

pub fn post(url: &str) -> Request {
    request("POST", url)
}

pub fn put(url: &str) -> Request {
    request("PUT", url)
}

pub fn patch(url: &str) -> Request {
    request("PATCH", url)
}

pub fn delete(url: &str) -> Request {
    request("DELETE", url)
}

impl Request {
    /// A header; several of one name are sent as given.
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.0.headers.push((name.to_string(), value.to_string()));
        self
    }

    /// `Authorization: Bearer <token>`.
    pub fn bearer(self, token: &str) -> Self {
        self.header("Authorization", &format!("Bearer {token}"))
    }

    /// The body as it is.
    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.0.body = Some(body.into());
        self
    }

    /// A JSON body, with `Content-Type: application/json`. A value that doesn't serialize leaves
    /// the request without a body (the plugin's log says why).
    pub fn json<T: Serialize + ?Sized>(self, value: &T) -> Self {
        match serde_json::to_vec(value) {
            Ok(body) => self.header("Content-Type", "application/json").body(body),
            Err(err) => {
                crate::log::warn(&format!("http: the JSON body doesn't serialize: {err}"));
                self
            }
        }
    }

    /// A form's body (`application/x-www-form-urlencoded`): what OAuth's token endpoints take.
    pub fn form(self, fields: &[(&str, &str)]) -> Self {
        self.header("Content-Type", "application/x-www-form-urlencoded")
            .body(crate::url::pairs(fields))
    }

    /// Gives up after this long (30 s by default).
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.0.timeout_ms = Some(timeout.as_millis().min(u32::MAX as u128) as u32);
        self
    }

    /// A redirect comes back as the response (3xx with `Location`) instead of being followed.
    pub fn no_redirects(mut self) -> Self {
        self.0.follow_redirects = false;
        self
    }

    /// Makes the request and waits for the whole response (at most 64 MB of body).
    pub fn fetch(&self) -> Result<Response, HttpError> {
        raw::fetch(&self.0)
    }

    /// Starts the request in the background and returns its id: the response comes as
    /// `Event::HttpResponse`, `Event::HttpBody` (chunks, any size in all) and `Event::HttpDone`.
    pub fn start(&self) -> u64 {
        raw::start(&self.0)
    }

    /// The request as the API has it.
    pub fn as_raw(&self) -> &raw::Request {
        &self.0
    }
}

/// Stops a started request: `Event::HttpDone` comes with [`HttpError::Cancelled`].
pub fn cancel(request: u64) {
    raw::cancel(request)
}

/// `url` with a query of `pairs`, each encoded: `with_query("https://oauth.example.com/authorize",
/// &[("client_id", id), ("state", state)])`. A URL with a query already gets `&` and the pairs.
pub fn with_query(url: &str, pairs: &[(&str, &str)]) -> String {
    if pairs.is_empty() {
        return url.to_string();
    }
    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}{}", crate::url::pairs(pairs))
}

/// The first header of that name (names compare without case).
pub(crate) fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(header, _)| header.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

impl Response {
    /// A 2xx status.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// The first header of that name (names compare without case).
    pub fn header(&self, name: &str) -> Option<&str> {
        header(&self.headers, name)
    }

    /// The body as text (invalid UTF-8 replaced).
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The body as JSON.
    pub fn json<T: DeserializeOwned>(&self) -> Result<T, String> {
        serde_json::from_slice(&self.body).map_err(|err| format!("Not the JSON expected: {err}"))
    }
}

impl ResponseHead {
    /// A 2xx status.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// The first header of that name (names compare without case).
    pub fn header(&self, name: &str) -> Option<&str> {
        header(&self.headers, name)
    }
}

impl HttpError {
    /// The error in words, for the user: "Couldn't connect: …". (`Display` gives the variant as
    /// it is, as the generated bindings make it.)
    pub fn message(&self) -> String {
        match self {
            HttpError::Denied(message) | HttpError::Invalid(message) => message.clone(),
            HttpError::Connection(message) => format!("Couldn't connect: {message}"),
            HttpError::TimedOut => "The request timed out".into(),
            HttpError::Cancelled => "The request was cancelled".into(),
            HttpError::TooLarge => "The response is larger than 64 MB".into(),
        }
    }
}

/// Server-sent events (`text/event-stream`) of a started request: feed it the chunks of
/// `Event::HttpBody`, and it returns the events completed by each.
///
/// ```ignore
/// Event::HttpBody((id, chunk)) if Some(id) == self.stream => {
///     for event in self.events.feed(&chunk) {
///         self.show(&event.data);
///     }
/// }
/// ```
#[derive(Debug, Default)]
pub struct EventStream {
    /// Bytes after the last newline: a line still coming.
    partial: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
    id: Option<String>,
}

/// One server-sent event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// The `event:` field; none — a plain message.
    pub event: Option<String>,
    /// The `data:` lines, joined with newlines.
    pub data: String,
    /// The last `id:` seen.
    pub id: Option<String>,
}

impl EventStream {
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes the next chunk of the body; returns the events it completes. Lines end at `\n`
    /// (`\r\n` too); an event ends at an empty line.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.partial.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(newline) = self.partial.iter().position(|&byte| byte == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=newline).collect();
            let line = String::from_utf8_lossy(&line[..line.len() - 1]);
            let line = line.strip_suffix('\r').unwrap_or(&line);
            if let Some(event) = self.line(line) {
                events.push(event);
            }
        }
        events
    }

    fn line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            if self.data.is_empty() {
                self.event = None;
                return None;
            }
            return Some(SseEvent {
                event: self.event.take(),
                data: std::mem::take(&mut self.data).join("\n"),
                id: self.id.clone(),
            });
        }
        if line.starts_with(':') {
            // A comment: a keep-alive.
            return None;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => self.event = Some(value.to_string()),
            "data" => self.data.push(value.to_string()),
            "id" => self.id = Some(value.to_string()),
            _ => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_requests() {
        let request = post("https://oauth.example.com/token")
            .form(&[("grant_type", "authorization_code"), ("code", "a b")])
            .bearer("t0k")
            .timeout(Duration::from_secs(5))
            .no_redirects();
        let raw = request.as_raw();
        assert_eq!(raw.method, "POST");
        assert_eq!(
            raw.body.as_deref(),
            Some(&b"grant_type=authorization_code&code=a%20b"[..])
        );
        assert_eq!(
            header(&raw.headers, "content-type"),
            Some("application/x-www-form-urlencoded")
        );
        assert_eq!(header(&raw.headers, "Authorization"), Some("Bearer t0k"));
        assert_eq!(raw.timeout_ms, Some(5000));
        assert!(!raw.follow_redirects);
        let json = put("https://x").json(&serde_json::json!({"a": 1}));
        assert_eq!(json.as_raw().body.as_deref(), Some(&br#"{"a":1}"#[..]));
    }

    #[test]
    fn queries() {
        assert_eq!(
            with_query("https://x/authorize", &[("scope", "a b"), ("state", "1")]),
            "https://x/authorize?scope=a%20b&state=1"
        );
        assert_eq!(with_query("https://x/?a=1", &[("b", "2")]), "https://x/?a=1&b=2");
        assert_eq!(with_query("https://x/", &[]), "https://x/");
    }

    #[test]
    fn responses() {
        let response = Response {
            status: 201,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: br#"{"ok":true}"#.to_vec(),
        };
        assert!(response.is_success());
        assert_eq!(response.header("content-type"), Some("application/json"));
        let value: serde_json::Value = response.json().unwrap();
        assert_eq!(value["ok"], true);
        assert!(response.json::<Vec<u8>>().is_err());
        assert_eq!(
            HttpError::Connection("refused".into()).message(),
            "Couldn't connect: refused"
        );
    }

    #[test]
    fn server_sent_events() {
        let mut stream = EventStream::new();
        assert!(stream.feed(b": keep-alive\n\ndata: one\n").is_empty());
        let events = stream.feed(b"\nevent: tick\r\ndata: tw");
        assert_eq!(
            events,
            [SseEvent {
                event: None,
                data: "one".into(),
                id: None
            }]
        );
        let events = stream.feed("о\ndata:three\nid: 7\n\n".as_bytes());
        assert_eq!(
            events,
            [SseEvent {
                event: Some("tick".into()),
                data: "twо\nthree".into(),
                id: Some("7".into())
            }]
        );
    }
}
