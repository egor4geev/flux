//! `http`: requests to web services, as the manifest's `network` permission allows (each request
//! and each redirect goes only to the hosts it lists). `fetch` waits on the plugin's thread (the
//! wait doesn't count toward its time limit); `start` runs on a thread of its own and posts
//! `http-response`, `http-body` and `http-done` events into the plugin's queue. Every request is a
//! line in the plugin's log: the method, the URL without its query, the status, the time and the
//! size — never headers or bodies, they carry tokens. The plugin's requests are cancelled when it
//! stops ([`Requests`]).
//!
//! The client is ureq: synchronous, TLS by rustls, certificates checked by the system's verifier
//! (Security.framework on macOS: a company's root certificates work). Redirects are followed here,
//! hop by hop, so that each one goes through the permission check; credentials don't follow a
//! redirect to another host. A cancel reaches a blocked read within [`SLICE`]: the connection is
//! read in slices that look at the request's cancel flag ([`Cancellable`]).

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ureq::http::{HeaderName, HeaderValue, Method, Request, Response};
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
use ureq::unversioned::resolver::DefaultResolver;
use ureq::unversioned::transport::time::Duration as Wait;
use ureq::unversioned::transport::{
    Buffers, ConnectProxyConnector, ConnectionDetails, Connector, NextTimeout, RustlsConnector,
    TcpConnector, Transport,
};
use ureq::{Agent, Body, RequestExt};
use url::Url;

use super::window::missing;
use crate::api::bindings::flux::plugin::http;
use crate::api::events::Event;
use crate::host::LogSignal;
use crate::log::{Level, PluginLog};
use crate::manifest::Permissions;
use crate::runtime::{EventSender, State};

/// How long a request may take when the plugin doesn't say.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// The longest a plugin may ask for.
const MAX_TIMEOUT: Duration = Duration::from_secs(600);
/// The largest body `fetch` takes (1 MB in tests); `start` streams any size.
const MAX_BODY: u64 = if cfg!(test) { 1 << 20 } else { 64 << 20 };
/// The largest chunk of a body in one `http-body` event.
const CHUNK: usize = 64 << 10;
/// Redirects followed for one request.
const MAX_REDIRECTS: u32 = 10;
/// A blocked read looks at the cancel flag this often.
const SLICE: Duration = Duration::from_millis(100);

/// The plugin's started requests (and its client, made on the first request): cancelled when the
/// plugin stops — dropped with its store.
#[derive(Default)]
pub(crate) struct Requests {
    agent: Option<Agent>,
    /// The cancel flags of the started requests that haven't ended, by id.
    active: Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>>,
}

impl Requests {
    fn agent(&mut self) -> Agent {
        self.agent.get_or_insert_with(new_agent).clone()
    }
}

impl Drop for Requests {
    fn drop(&mut self) {
        for cancel in self.active.lock().unwrap().values() {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

impl http::Host for State {
    fn fetch(&mut self, request: http::Request) -> Result<http::Response, http::HttpError> {
        let started = Instant::now();
        let result = self.fetch_now(request);
        self.host.waited += started.elapsed();
        result
    }

    fn start(&mut self, request: http::Request) -> u64 {
        let id = self.host.next_id();
        let host = &mut self.host;
        let line = Line::of(&request);
        let prepared = match prepare(request, &host.entry.manifest.permissions) {
            Ok(prepared) => prepared,
            Err(err) => {
                line.write(&host.log, &host.signal, None, Some(&err), 0, None);
                host.events.send(Event::HttpDone((id, Err(err))));
                return id;
            }
        };
        let cancel = Arc::new(AtomicBool::new(false));
        host.requests
            .active
            .lock()
            .unwrap()
            .insert(id, cancel.clone());
        let stream = Stream {
            id,
            agent: host.requests.agent(),
            permissions: host.entry.manifest.permissions.clone(),
            events: host.events.clone(),
            log: host.log.clone(),
            signal: host.signal.clone(),
            active: host.requests.active.clone(),
            cancel,
            line,
        };
        let spawned = std::thread::Builder::new()
            .name(format!("plugin {} request", host.id))
            .spawn(move || stream.run(prepared));
        if let Err(err) = spawned {
            host.requests.active.lock().unwrap().remove(&id);
            let err = http::HttpError::Connection(format!("Couldn't start a thread: {err}"));
            host.events.send(Event::HttpDone((id, Err(err))));
        }
        id
    }

    fn cancel(&mut self, request: u64) {
        if let Some(cancel) = self.host.requests.active.lock().unwrap().get(&request) {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

impl State {
    fn fetch_now(&mut self, request: http::Request) -> Result<http::Response, http::HttpError> {
        let host = &mut self.host;
        let line = Line::of(&request);
        let started = Instant::now();
        let result = (|| {
            let prepared = prepare(request, &host.entry.manifest.permissions)?;
            let agent = host.requests.agent();
            // Waits on the plugin's thread: the instance's stop cancels it.
            let cancel = host.cancel.clone();
            with_cancel(cancel.clone(), || {
                let deadline = started + prepared.timeout;
                let (response, _) = call(
                    &agent,
                    prepared,
                    &host.entry.manifest.permissions,
                    deadline,
                    false,
                )
                .map_err(|err| failure(err, &cancel))?;
                let (parts, mut body) = response.into_parts();
                let body = body
                    .with_config()
                    .limit(MAX_BODY)
                    .read_to_vec()
                    .map_err(|err| failure(Failure::Ureq(err), &cancel))?;
                Ok(http::Response {
                    status: parts.status.as_u16(),
                    headers: header_list(&parts.headers),
                    body,
                })
            })
        })();
        let (status, size) = match &result {
            Ok(response) => (Some(response.status), response.body.len()),
            Err(_) => (None, 0),
        };
        line.write(
            &host.log,
            &host.signal,
            status,
            result.as_ref().err(),
            size,
            Some(started.elapsed()),
        );
        result
    }
}

/// A request checked and ready: the URL is http or https, to a host the manifest allows, the
/// method and the headers are valid.
struct Prepared {
    method: Method,
    url: Url,
    headers: Vec<(HeaderName, HeaderValue)>,
    body: Option<Vec<u8>>,
    timeout: Duration,
    follow: bool,
}

fn prepare(request: http::Request, permissions: &Permissions) -> Result<Prepared, http::HttpError> {
    let invalid = |message: String| http::HttpError::Invalid(message);
    let url = parse_url(&request.url)?;
    check_host(&url, permissions)?;
    let method = Method::from_bytes(request.method.trim().to_ascii_uppercase().as_bytes())
        .map_err(|_| invalid(format!("Not a method: \"{}\"", request.method)))?;
    let headers = request
        .headers
        .iter()
        .map(|(name, value)| {
            let header = HeaderName::from_bytes(name.trim().as_bytes())
                .map_err(|_| invalid(format!("Not a header name: \"{name}\"")))?;
            let value = HeaderValue::from_str(value)
                .map_err(|_| invalid(format!("Not a value of the header {name}")))?;
            Ok((header, value))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let timeout = request
        .timeout_ms
        .map(|ms| Duration::from_millis(ms.into()))
        .filter(|timeout| !timeout.is_zero())
        .unwrap_or(DEFAULT_TIMEOUT)
        .min(MAX_TIMEOUT);
    Ok(Prepared {
        method,
        url,
        headers,
        body: request.body,
        timeout,
        follow: request.follow_redirects,
    })
}

fn parse_url(text: &str) -> Result<Url, http::HttpError> {
    let url = Url::parse(text.trim())
        .map_err(|err| http::HttpError::Invalid(format!("Not a URL: \"{text}\" ({err})")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(http::HttpError::Invalid(format!(
            "Only http and https URLs: \"{text}\""
        )));
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(http::HttpError::Invalid(format!("No host in \"{text}\"")));
    }
    Ok(url)
}

/// The manifest's `network` permission for the URL's host.
fn check_host(url: &Url, permissions: &Permissions) -> Result<(), http::HttpError> {
    let host = url.host_str().unwrap_or_default();
    if permissions.allows_host(host) {
        return Ok(());
    }
    let name = host.trim_start_matches('[').trim_end_matches(']');
    Err(http::HttpError::Denied(missing(
        &format!("send requests to {name}"),
        &format!("network = [\"{name}\"]"),
    )))
}

/// Why a call failed before an [`http::HttpError`] is made of it (the cancel flag decides first).
enum Failure {
    Ureq(ureq::Error),
    Io(std::io::Error),
    Api(http::HttpError),
}

impl From<http::HttpError> for Failure {
    fn from(err: http::HttpError) -> Self {
        Failure::Api(err)
    }
}

fn failure(failure: Failure, cancel: &AtomicBool) -> http::HttpError {
    if cancel.load(Ordering::Relaxed) {
        return http::HttpError::Cancelled;
    }
    match failure {
        Failure::Api(err) => err,
        Failure::Ureq(ureq::Error::Timeout(_)) => http::HttpError::TimedOut,
        Failure::Ureq(ureq::Error::BodyExceedsLimit(_)) => http::HttpError::TooLarge,
        Failure::Ureq(ureq::Error::BadUri(err)) => http::HttpError::Invalid(err),
        Failure::Ureq(ureq::Error::Http(err)) => http::HttpError::Invalid(err.to_string()),
        Failure::Ureq(ureq::Error::HostNotFound) => {
            http::HttpError::Connection("The host isn't found".into())
        }
        Failure::Ureq(ureq::Error::Io(err)) | Failure::Io(err) => io_failure(err),
        Failure::Ureq(err) => http::HttpError::Connection(err.to_string()),
    }
}

fn io_failure(err: std::io::Error) -> http::HttpError {
    // The body reader wraps ureq's own errors into io errors.
    let err = match err.downcast::<ureq::Error>() {
        Ok(ureq::Error::Timeout(_)) => return http::HttpError::TimedOut,
        Ok(ureq::Error::BodyExceedsLimit(_)) => return http::HttpError::TooLarge,
        Ok(err) => return http::HttpError::Connection(err.to_string()),
        Err(err) => err,
    };
    match err.kind() {
        std::io::ErrorKind::TimedOut => http::HttpError::TimedOut,
        _ => http::HttpError::Connection(err.to_string()),
    }
}

/// Makes the request, following its redirects (each one checked against the manifest): the last
/// response, with its body not read yet, and the number of redirects. `streaming` — the body may
/// take any time (`start`): the deadline holds until the response's head comes.
fn call(
    agent: &Agent,
    mut hop: Prepared,
    permissions: &Permissions,
    deadline: Instant,
    streaming: bool,
) -> Result<(Response<Body>, u32), Failure> {
    let mut redirects = 0;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Failure::Api(http::HttpError::TimedOut));
        }
        let response = send(agent, &hop, left, streaming).map_err(Failure::Ureq)?;
        let status = response.status().as_u16();
        let location = response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let (true, Some(location), 301 | 302 | 303 | 307 | 308) = (hop.follow, location, status)
        else {
            return Ok((response, redirects));
        };
        // The redirect's own body goes with the response.
        drop(response);
        redirects += 1;
        if redirects > MAX_REDIRECTS {
            return Err(Failure::Api(http::HttpError::Invalid(format!(
                "More than {MAX_REDIRECTS} redirects"
            ))));
        }
        let next = hop.url.join(&location).map_err(|err| {
            Failure::Api(http::HttpError::Invalid(format!(
                "Not a URL to redirect to: \"{location}\" ({err})"
            )))
        })?;
        let next = parse_url(next.as_str())?;
        check_host(&next, permissions)?;
        // 303, and 301 / 302 after a POST, become a GET without a body, as browsers do.
        if status == 303 || (matches!(status, 301 | 302) && hop.method == Method::POST) {
            if hop.method != Method::HEAD {
                hop.method = Method::GET;
            }
            hop.body = None;
            hop.headers.retain(|(name, _)| {
                !matches!(
                    name.as_str(),
                    "content-type" | "content-length" | "content-encoding"
                )
            });
        }
        // Credentials stay with the host they were meant for.
        let same_origin = next.scheme() == hop.url.scheme()
            && next.host_str() == hop.url.host_str()
            && next.port_or_known_default() == hop.url.port_or_known_default();
        if !same_origin {
            hop.headers.retain(|(name, _)| {
                !matches!(
                    name.as_str(),
                    "authorization" | "cookie" | "proxy-authorization"
                )
            });
        }
        hop.url = next;
    }
}

/// One hop: the request as it is, redirects not followed.
fn send(
    agent: &Agent,
    hop: &Prepared,
    left: Duration,
    streaming: bool,
) -> Result<Response<Body>, ureq::Error> {
    let mut builder = Request::builder()
        .method(hop.method.clone())
        .uri(hop.url.as_str());
    for (name, value) in &hop.headers {
        builder = builder.header(name, value);
    }
    let connect = left.min(Duration::from_secs(15));
    match &hop.body {
        Some(body) => {
            let request = builder.body(body.clone())?;
            let config = request
                .with_agent(agent)
                .configure()
                .timeout_connect(Some(connect));
            if streaming {
                config
                    .timeout_send_request(Some(left))
                    .timeout_send_body(Some(left))
                    .timeout_recv_response(Some(left))
                    .run()
            } else {
                config.timeout_global(Some(left)).run()
            }
        }
        None => {
            let request = builder.body(())?;
            let config = request
                .with_agent(agent)
                .configure()
                .timeout_connect(Some(connect));
            if streaming {
                config
                    .timeout_send_request(Some(left))
                    .timeout_recv_response(Some(left))
                    .run()
            } else {
                config.timeout_global(Some(left)).run()
            }
        }
    }
}

fn header_list(headers: &ureq::http::HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect()
}

/// A started request on its own thread.
struct Stream {
    id: u64,
    agent: Agent,
    permissions: Permissions,
    events: EventSender,
    log: PluginLog,
    signal: Arc<LogSignal>,
    active: Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>>,
    cancel: Arc<AtomicBool>,
    line: Line,
}

impl Stream {
    fn run(self, prepared: Prepared) {
        let started = Instant::now();
        let mut status = None;
        let mut size = 0;
        let result = with_cancel(self.cancel.clone(), || {
            let deadline = started + prepared.timeout;
            let (response, _) = call(&self.agent, prepared, &self.permissions, deadline, true)
                .map_err(|err| failure(err, &self.cancel))?;
            let (parts, body) = response.into_parts();
            status = Some(parts.status.as_u16());
            let head = http::ResponseHead {
                request: self.id,
                status: parts.status.as_u16(),
                headers: header_list(&parts.headers),
            };
            if !self.events.send(Event::HttpResponse(head)) {
                // The plugin is gone: nobody to tell.
                return Err(http::HttpError::Cancelled);
            }
            let mut reader = body.into_reader();
            let mut buffer = vec![0; CHUNK];
            loop {
                if self.cancel.load(Ordering::Relaxed) {
                    return Err(http::HttpError::Cancelled);
                }
                let read = reader
                    .read(&mut buffer)
                    .map_err(|err| failure(Failure::Io(err), &self.cancel))?;
                if read == 0 {
                    return Ok(());
                }
                size += read;
                let chunk = buffer[..read].to_vec();
                if !self.events.send(Event::HttpBody((self.id, chunk))) {
                    return Err(http::HttpError::Cancelled);
                }
            }
        });
        self.active.lock().unwrap().remove(&self.id);
        self.line.write(
            &self.log,
            &self.signal,
            status,
            result.as_ref().err(),
            size,
            Some(started.elapsed()),
        );
        self.events.send(Event::HttpDone((self.id, result)));
    }
}

/// The plugin's log line about a request: what was asked for (the URL without its query), then
/// what came of it.
struct Line {
    method: String,
    url: String,
}

impl Line {
    fn of(request: &http::Request) -> Self {
        let url = match Url::parse(request.url.trim()) {
            Ok(mut url) => {
                url.set_query(None);
                url.set_fragment(None);
                let _ = url.set_username("");
                let _ = url.set_password(None);
                url.to_string()
            }
            Err(_) => request.url.chars().take(200).collect(),
        };
        Line {
            method: request.method.trim().to_ascii_uppercase(),
            url,
        }
    }

    /// "GET https://api.example.com/v1/issues → 200 · 135 ms · 4.2 KB"; a stream that ended
    /// otherwise than by its end: "→ 200, cancelled · …".
    fn write(
        &self,
        log: &PluginLog,
        signal: &LogSignal,
        status: Option<u16>,
        error: Option<&http::HttpError>,
        size: usize,
        time: Option<Duration>,
    ) {
        let mut parts = Vec::new();
        if let Some(status) = status {
            parts.push(status.to_string());
        }
        if let Some(error) = error {
            parts.push(describe(error));
        }
        let mut text = format!("{} {} → {}", self.method, self.url, parts.join(", "));
        let failed = !matches!(error, None | Some(http::HttpError::Cancelled));
        let level = if failed || status.is_some_and(|status| status >= 400) {
            Level::Warn
        } else {
            Level::Info
        };
        if let Some(time) = time {
            text.push_str(&format!(" · {} ms", time.as_millis()));
        }
        if size > 0 {
            text.push_str(&format!(" · {}", bytes(size)));
        }
        log.write(level, &text);
        signal.logged();
    }
}

fn describe(err: &http::HttpError) -> String {
    match err {
        http::HttpError::Denied(text) => format!("denied: {text}"),
        http::HttpError::Invalid(text) => format!("invalid: {text}"),
        http::HttpError::Connection(text) => format!("failed: {text}"),
        http::HttpError::TimedOut => "timed out".into(),
        http::HttpError::Cancelled => "cancelled".into(),
        http::HttpError::TooLarge => "too large".into(),
    }
}

/// "512 B", "4.2 KB", "1.5 MB".
fn bytes(size: usize) -> String {
    match size {
        0..1024 => format!("{size} B"),
        1024..1_048_576 => format!("{:.1} KB", size as f64 / 1024.),
        _ => format!("{:.1} MB", size as f64 / 1_048_576.),
    }
}

// --- The client ---

/// A client of one plugin: redirects are followed by [`call`], statuses are responses, any
/// method goes (WebDAV's PROPFIND), the system's certificates, connections read in slices
/// ([`Cancellable`]).
fn new_agent() -> Agent {
    let config = Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .save_redirect_history(false)
        .allow_non_standard_methods(true)
        .user_agent(format!("Flux/{}", env!("CARGO_PKG_VERSION")))
        .tls_config(
            TlsConfig::builder()
                .provider(TlsProvider::Rustls)
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .build();
    let connector =
        ().chain(ConnectProxyConnector::default())
            .chain(TcpConnector::default())
            .chain(CancelConnector)
            .chain(RustlsConnector::default());
    Agent::with_parts(config, connector, DefaultResolver::default())
}

thread_local! {
    /// The cancel flag of the request this thread makes.
    static CANCEL: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
}

/// Runs `f` with `cancel` as the flag the connections of this thread look at.
fn with_cancel<T>(cancel: Arc<AtomicBool>, f: impl FnOnce() -> T) -> T {
    CANCEL.with(|flag| *flag.borrow_mut() = Some(cancel));
    let result = f();
    CANCEL.with(|flag| *flag.borrow_mut() = None);
    result
}

fn cancelled() -> bool {
    CANCEL.with(|flag| {
        flag.borrow()
            .as_ref()
            .is_some_and(|cancel| cancel.load(Ordering::Relaxed))
    })
}

fn cancelled_error() -> ureq::Error {
    ureq::Error::Io(std::io::Error::new(
        std::io::ErrorKind::Interrupted,
        "cancelled",
    ))
}

/// Wraps the TCP connection (under TLS): reads wait in slices of [`SLICE`] and give up once the
/// request is cancelled.
#[derive(Debug)]
struct CancelConnector;

impl<In: Transport> Connector<In> for CancelConnector {
    type Out = Cancellable<In>;

    fn connect(
        &self,
        _: &ConnectionDetails,
        chained: Option<In>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        Ok(chained.map(|inner| Cancellable { inner }))
    }
}

#[derive(Debug)]
struct Cancellable<T: Transport> {
    inner: T,
}

impl<T: Transport> Transport for Cancellable<T> {
    fn buffers(&mut self) -> &mut dyn Buffers {
        self.inner.buffers()
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        if cancelled() {
            return Err(cancelled_error());
        }
        self.inner.transmit_output(amount, timeout)
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        let deadline = match timeout.after {
            Wait::NotHappening => None,
            after => Some(Instant::now() + *after),
        };
        loop {
            if cancelled() {
                return Err(cancelled_error());
            }
            let left = deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
            let slice = left.map_or(SLICE, |left| left.min(SLICE));
            let result = self.inner.await_input(NextTimeout {
                after: Wait::Exact(slice.max(Duration::from_millis(1))),
                reason: timeout.reason,
            });
            match result {
                // A slice ran out, not the request's time.
                Err(ureq::Error::Timeout(_)) if left.is_none_or(|left| left > slice) => continue,
                result => return result,
            }
        }
    }

    fn is_open(&mut self) -> bool {
        self.inner.is_open()
    }

    fn is_tls(&self) -> bool {
        self.inner.is_tls()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_lines_have_no_query() {
        let request = http::Request {
            method: "get".into(),
            url: "https://user:secret@api.example.com/v1/issues?token=abc#top".into(),
            headers: Vec::new(),
            body: None,
            timeout_ms: None,
            follow_redirects: true,
        };
        let line = Line::of(&request);
        assert_eq!(line.method, "GET");
        assert_eq!(line.url, "https://api.example.com/v1/issues");
    }

    #[test]
    fn sizes() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(4300), "4.2 KB");
        assert_eq!(bytes(3 << 20), "3.0 MB");
    }

    #[test]
    fn urls_are_checked() {
        let permissions = Permissions {
            network: vec!["api.example.com".into()],
            ..Default::default()
        };
        let check = |url: &str| parse_url(url).and_then(|url| check_host(&url, &permissions));
        assert!(check("https://api.example.com/x").is_ok());
        assert!(matches!(
            check("ftp://api.example.com/x"),
            Err(http::HttpError::Invalid(_))
        ));
        assert!(matches!(
            check("not a url"),
            Err(http::HttpError::Invalid(_))
        ));
        match check("https://example.com/") {
            Err(http::HttpError::Denied(text)) => {
                assert!(text.contains("network = [\"example.com\"]"), "{text}")
            }
            other => panic!("{other:?}"),
        }
    }
}
