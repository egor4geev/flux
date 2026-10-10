//! The probe's commands for requests (`http`) and the plugin's server (`server`): `net:<name>`;
//! they report through notifications, as the other commands (their titles are what the tests
//! check).
//!
//! A request is described by the test in `net-request.txt` in the plugin's folder: the method, the
//! URL, `follow` (or anything else: redirects aren't followed), the timeout in milliseconds (or an
//! empty line), then the body (the rest of the file).

use std::cell::{Cell, RefCell};

use flux_plugin_api::Event;
use flux_plugin_api::host::http::{self, Request};
use flux_plugin_api::host::server::{self, ServerResponse, WsMessage};
use flux_plugin_api::host::storage;
use flux_plugin_api::notify;

thread_local! {
    /// The last started request: `net:cancel` cancels it.
    static LAST: Cell<u64> = const { Cell::new(0) };
    /// The body of the started requests so far.
    static BODY: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    /// The request whose stream stays open (`/forever`).
    static FOREVER: Cell<u64> = const { Cell::new(0) };
}

pub fn run(command: &str) {
    match command {
        "fetch" => match request() {
            Ok(request) => match http::fetch(&request) {
                Ok(response) => {
                    let test = response
                        .headers
                        .iter()
                        .find(|(name, _)| name.eq_ignore_ascii_case("x-test"))
                        .map_or("-", |(_, value)| value.as_str());
                    notify::info(&format!(
                        "fetch: {} x-test={test} {}",
                        response.status,
                        String::from_utf8_lossy(&response.body)
                    ));
                }
                Err(err) => {
                    notify::info(&format!("fetch failed: {err:?}"));
                }
            },
            Err(err) => {
                notify::info(&err);
            }
        },
        "start" => match request() {
            Ok(request) => {
                BODY.with(|body| body.borrow_mut().clear());
                LAST.set(http::start(&request));
            }
            Err(err) => {
                notify::info(&err);
            }
        },
        "cancel" => http::cancel(LAST.get()),
        "listen" => {
            let title = match server::listen(None) {
                Ok(port) => format!("listening: {port}"),
                Err(err) => format!("listen failed: {err}"),
            };
            notify::info(&title);
        }
        "stop" => {
            server::stop();
            notify::info("stopped");
        }
        _ => {}
    }
}

/// Handles the events of requests and of the server; true when the event was one of them.
pub fn on_event(event: &Event) -> bool {
    match event {
        Event::HttpResponse(head) => {
            notify::info(&format!("head: {}", head.status));
        }
        Event::HttpBody((_, chunk)) => {
            BODY.with(|body| body.borrow_mut().extend_from_slice(chunk));
            notify::info(&format!("body: {}", String::from_utf8_lossy(chunk)));
        }
        Event::HttpDone((_, result)) => {
            let title = match result {
                Ok(()) => {
                    let body =
                        BODY.with(|body| String::from_utf8_lossy(&body.borrow()).into_owned());
                    format!("done: ok {body}")
                }
                Err(err) => format!("done: {err:?}"),
            };
            notify::info(&title);
        }
        Event::ServerRequest(request) => {
            let path = request.path.split('?').next().unwrap_or_default();
            match path {
                "/hello" => server::respond(
                    request.id,
                    &ServerResponse {
                        status: 200,
                        headers: vec![
                            ("x-probe".into(), "yes".into()),
                            ("content-type".into(), "text/plain".into()),
                        ],
                        body: format!(
                            "hi {} {}",
                            request.method,
                            String::from_utf8_lossy(&request.body)
                        )
                        .into_bytes(),
                    },
                ),
                "/stream" => {
                    server::respond_stream(
                        request.id,
                        200,
                        &[("content-type".into(), "text/event-stream".into())],
                    );
                    for data in ["one", "two"] {
                        let _ =
                            server::send_chunk(request.id, format!("data: {data}\n\n").as_bytes());
                    }
                    server::end_stream(request.id);
                }
                "/forever" => {
                    server::respond_stream(
                        request.id,
                        200,
                        &[("content-type".into(), "text/event-stream".into())],
                    );
                    let _ = server::send_chunk(request.id, b"data: start\n\n");
                    FOREVER.set(request.id);
                }
                // No answer: the server answers 504 in the end.
                "/silent" => {}
                _ => server::respond(
                    request.id,
                    &ServerResponse {
                        status: 404,
                        headers: Vec::new(),
                        body: b"no such path".to_vec(),
                    },
                ),
            }
        }
        Event::StreamClosed(request) => {
            let which = if *request == FOREVER.get() {
                "forever"
            } else {
                "other"
            };
            notify::info(&format!("stream closed: {which}"));
        }
        Event::WsOpened(open) => {
            notify::info(&format!("ws open {}", open.path));
        }
        Event::WsMessage((connection, message)) => match message {
            WsMessage::Text(text) if text == "bye" => server::ws_close(*connection),
            WsMessage::Text(text) => {
                let _ = server::ws_send(*connection, &WsMessage::Text(format!("echo: {text}")));
            }
            WsMessage::Binary(bytes) => {
                let reversed: Vec<u8> = bytes.iter().rev().copied().collect();
                let _ = server::ws_send(*connection, &WsMessage::Binary(reversed));
            }
        },
        Event::WsClosed(_) => {
            notify::info("ws closed");
        }
        _ => return false,
    }
    true
}

/// The request the test described in `net-request.txt`.
fn request() -> Result<Request, String> {
    let file = format!("{}/net-request.txt", storage::data_dir());
    let text = std::fs::read_to_string(&file).map_err(|err| format!("no request: {err}"))?;
    let mut lines = text.splitn(5, '\n');
    let mut next = || lines.next().unwrap_or_default().to_string();
    let method = next();
    let url = next();
    let follow = next() == "follow";
    let timeout = next();
    let body = next();
    Ok(Request {
        method,
        url,
        headers: vec![("x-asked".into(), "by the probe".into())],
        body: (!body.is_empty()).then(|| body.into_bytes()),
        timeout_ms: timeout.trim().parse().ok(),
        follow_redirects: follow,
    })
}
