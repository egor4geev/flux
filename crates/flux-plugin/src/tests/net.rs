//! The runtime against the probe: requests (`http`) and the plugin's server (`server`). The probe's
//! commands are in `tests/fixtures/probe/src/net.rs`; `super::probe_with` makes a probe with the
//! permissions and the commands a test needs. Nothing leaves this Mac: the requests go to a small
//! server of the test's own on 127.0.0.1.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Map;

use super::{Probe, probe_with, start};

const COMMANDS: &[&str] = &[
    "net:fetch",
    "net:start",
    "net:cancel",
    "net:listen",
    "net:stop",
];

/// A running probe allowed to reach this Mac and to run a server, and its folder.
fn net_probe(id: &str, permissions: &str) -> Option<(Probe, PathBuf)> {
    let entry = probe_with(id, permissions, COMMANDS)?;
    let probe = start(entry, None, Map::new());
    Some((probe, crate::paths::data_dir(id)))
}

const LOCAL: &str = "project = \"none\"\nnetwork = [\"localhost\"]\nserver = true";

/// Describes the probe's next request.
fn ask(data: &Path, method: &str, url: &str, follow: bool, timeout_ms: Option<u32>, body: &str) {
    let follow = if follow { "follow" } else { "stay" };
    let timeout = timeout_ms.map(|ms| ms.to_string()).unwrap_or_default();
    std::fs::write(
        data.join("net-request.txt"),
        format!("{method}\n{url}\n{follow}\n{timeout}\n{body}"),
    )
    .unwrap();
}

impl Probe {
    /// Runs a command that reports later (or never).
    fn send(&mut self, command: &str) {
        self.instance.run_command(command, super::palette());
    }

    /// The next notification that starts with `prefix`; the others (the chunks of a body) are
    /// skipped.
    fn wait_for(&mut self, prefix: &str) -> String {
        loop {
            let title = self.notification();
            if title.starts_with(prefix) {
                return title;
            }
        }
    }
}

// --- A server of the test's own, for the client ---

/// Starts the test's server: its port. One request per connection.
fn test_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || answer(stream));
        }
    });
    port
}

fn answer(stream: TcpStream) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let mut words = line.split_whitespace();
    let method = words.next().unwrap_or_default().to_string();
    let path = words.next().unwrap_or_default().to_string();
    let mut length = 0;
    let mut asked = String::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
            break;
        }
        let (name, value) = header.split_once(':').unwrap_or_default();
        match name.trim().to_ascii_lowercase().as_str() {
            "content-length" => length = value.trim().parse().unwrap_or(0),
            "x-asked" => asked = value.trim().to_string(),
            _ => {}
        }
    }
    let mut body = vec![0; length];
    let _ = reader.read_exact(&mut body);
    let body = String::from_utf8_lossy(&body).into_owned();
    let mut stream = stream;
    let mut whole = |status: &str, headers: &str, body: &[u8]| {
        let head = format!(
            "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(body);
    };
    match path.split('?').next().unwrap_or_default() {
        "/hello" => whole("200 OK", "X-Test: yes\r\n", b"hello"),
        "/echo" => whole("200 OK", "", format!("{method} {asked} {body}").as_bytes()),
        "/redirect-local" => whole("302 Found", "Location: /hello\r\n", b""),
        "/redirect-away" => whole("302 Found", "Location: http://example.com/\r\n", b""),
        "/redirect-post" => whole("303 See Other", "Location: /echo\r\n", b""),
        "/slow" => {
            std::thread::sleep(Duration::from_secs(3));
            whole("200 OK", "", b"late");
        }
        "/big" => whole("200 OK", "", &vec![b'x'; 2 << 20]),
        "/stream" | "/endless" => {
            let endless = path == "/endless";
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            );
            for n in 0.. {
                let part = if endless {
                    "tick"
                } else {
                    ["one", "two", "three"][n]
                };
                let chunk = format!("{:x}\r\n{part}\r\n", part.len());
                if stream.write_all(chunk.as_bytes()).is_err() || stream.flush().is_err() {
                    return;
                }
                if !endless && n == 2 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(if endless { 50 } else { 100 }));
            }
            let _ = stream.write_all(b"0\r\n\r\n");
        }
        _ => whole("404 Not Found", "", b"nothing"),
    }
}

// --- The client ---

#[test]
fn fetches_from_allowed_hosts_only() {
    let Some((mut probe, data)) = net_probe("test.net-fetch", LOCAL) else {
        return;
    };
    let port = test_server();
    let base = format!("http://127.0.0.1:{port}");

    ask(
        &data,
        "GET",
        &format!("{base}/hello?token=secret"),
        true,
        None,
        "",
    );
    assert_eq!(probe.run("net:fetch"), "fetch: 200 x-test=yes hello");
    // A custom method with a body, the plugin's headers.
    ask(
        &data,
        "PROPFIND",
        &format!("{base}/echo"),
        true,
        None,
        "<propfind/>",
    );
    assert_eq!(
        probe.run("net:fetch"),
        "fetch: 200 x-test=- PROPFIND by the probe <propfind/>"
    );
    ask(&data, "GET", &format!("{base}/nope"), true, None, "");
    assert_eq!(probe.run("net:fetch"), "fetch: 404 x-test=- nothing");

    // Not allowed: the host, and a redirect to another one.
    ask(&data, "GET", "http://10.0.0.1/", true, None, "");
    let denied = probe.run("net:fetch");
    assert!(
        denied.starts_with("fetch failed: HttpError::Denied("),
        "{denied}"
    );
    assert!(denied.contains("network = [\\\"10.0.0.1\\\"]"), "{denied}");
    ask(
        &data,
        "GET",
        &format!("{base}/redirect-away"),
        true,
        None,
        "",
    );
    let denied = probe.run("net:fetch");
    assert!(
        denied.starts_with("fetch failed: HttpError::Denied("),
        "{denied}"
    );
    assert!(denied.contains("example.com"), "{denied}");
    ask(&data, "GET", "ftp://127.0.0.1/", true, None, "");
    let invalid = probe.run("net:fetch");
    assert!(
        invalid.starts_with("fetch failed: HttpError::Invalid("),
        "{invalid}"
    );

    // The log has the request without its query.
    let log = probe.log_text();
    assert!(log.contains(&format!("GET {base}/hello → 200")), "{log}");
    assert!(!log.contains("secret"), "{log}");
}

#[test]
fn follows_redirects_as_asked() {
    let Some((mut probe, data)) = net_probe("test.net-redirects", LOCAL) else {
        return;
    };
    let base = format!("http://127.0.0.1:{}", test_server());
    ask(
        &data,
        "GET",
        &format!("{base}/redirect-local"),
        true,
        None,
        "",
    );
    assert_eq!(probe.run("net:fetch"), "fetch: 200 x-test=yes hello");
    ask(
        &data,
        "GET",
        &format!("{base}/redirect-local"),
        false,
        None,
        "",
    );
    assert_eq!(probe.run("net:fetch"), "fetch: 302 x-test=- ");
    // 303: a GET without the body.
    ask(
        &data,
        "POST",
        &format!("{base}/redirect-post"),
        true,
        None,
        "data",
    );
    assert_eq!(
        probe.run("net:fetch"),
        "fetch: 200 x-test=- GET by the probe "
    );
}

#[test]
fn gives_up_on_time_and_on_size() {
    let Some((mut probe, data)) = net_probe("test.net-limits", LOCAL) else {
        return;
    };
    let base = format!("http://127.0.0.1:{}", test_server());
    ask(&data, "GET", &format!("{base}/slow"), true, Some(300), "");
    let started = Instant::now();
    assert_eq!(probe.run("net:fetch"), "fetch failed: HttpError::TimedOut");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    // Tests take 1 MB at most.
    ask(&data, "GET", &format!("{base}/big"), true, None, "");
    assert_eq!(probe.run("net:fetch"), "fetch failed: HttpError::TooLarge");
}

#[test]
fn streams_a_started_request() {
    let Some((mut probe, data)) = net_probe("test.net-stream", LOCAL) else {
        return;
    };
    let base = format!("http://127.0.0.1:{}", test_server());
    ask(&data, "GET", &format!("{base}/stream"), true, None, "");
    probe.send("net:start");
    assert_eq!(probe.notification(), "head: 200");
    // The chunks come as they are sent, not all at the end.
    assert_eq!(probe.notification(), "body: one");
    assert_eq!(probe.notification(), "body: two");
    assert_eq!(probe.notification(), "body: three");
    assert_eq!(probe.notification(), "done: ok onetwothree");

    // A failure before the head: only the end.
    ask(&data, "GET", "http://10.0.0.1/", true, None, "");
    probe.send("net:start");
    let done = probe.notification();
    assert!(done.starts_with("done: HttpError::Denied("), "{done}");
}

#[test]
fn cancels_a_started_request() {
    let Some((mut probe, data)) = net_probe("test.net-cancel", LOCAL) else {
        return;
    };
    let base = format!("http://127.0.0.1:{}", test_server());
    ask(&data, "GET", &format!("{base}/endless"), true, None, "");
    probe.send("net:start");
    assert_eq!(probe.notification(), "head: 200");
    assert_eq!(probe.notification(), "body: tick");
    let cancelled = Instant::now();
    probe.send("net:cancel");
    assert_eq!(probe.wait_for("done"), "done: HttpError::Cancelled");
    assert!(
        cancelled.elapsed() < Duration::from_secs(1),
        "{:?}",
        cancelled.elapsed()
    );
}

// --- The server ---

/// The probe's server: its port.
fn listen(probe: &mut Probe) -> u16 {
    let title = probe.run("net:listen");
    title
        .strip_prefix("listening: ")
        .and_then(|port| port.parse().ok())
        .unwrap_or_else(|| panic!("{title}"))
}

/// Sends `request` and reads the answer until the server closes the connection.
fn exchange(port: u16, request: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut answer = Vec::new();
    let _ = stream.read_to_end(&mut answer);
    String::from_utf8_lossy(&answer).into_owned()
}

#[test]
fn a_server_needs_the_permission() {
    let Some((mut probe, _)) = net_probe("test.net-no-server", "project = \"none\"") else {
        return;
    };
    let title = probe.run("net:listen");
    assert!(title.contains("server = true"), "{title}");
}

#[test]
fn serves_requests_to_the_plugin() {
    let Some((mut probe, _)) = net_probe("test.net-serve", LOCAL) else {
        return;
    };
    let port = listen(&mut probe);
    let host = format!("Host: 127.0.0.1:{port}\r\n");

    let answer = exchange(
        port,
        &format!("GET /hello HTTP/1.1\r\n{host}Connection: close\r\n\r\n"),
    );
    assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
    assert!(answer.contains("x-probe: yes\r\n"), "{answer}");
    assert!(answer.ends_with("\r\n\r\nhi GET "), "{answer}");

    let answer = exchange(
        port,
        &format!(
            "POST /hello?x=1 HTTP/1.1\r\nHost: localhost:{port}\r\nContent-Length: 7\r\nConnection: \
             close\r\n\r\npayload"
        ),
    );
    assert!(answer.ends_with("hi POST payload"), "{answer}");

    // Two requests on one connection.
    let answer = exchange(
        port,
        &format!(
            "GET /hello HTTP/1.1\r\n{host}\r\nGET /nope HTTP/1.1\r\n{host}Connection: close\r\n\r\n"
        ),
    );
    assert!(answer.starts_with("HTTP/1.1 200 OK"), "{answer}");
    assert!(answer.contains("HTTP/1.1 404 Not Found"), "{answer}");

    // A page that reaches the server through a name of its own.
    let answer = exchange(port, "GET /hello HTTP/1.1\r\nHost: evil.example:80\r\n\r\n");
    assert!(answer.starts_with("HTTP/1.1 403 Forbidden"), "{answer}");

    // No answer from the plugin.
    let answer = exchange(port, &format!("GET /silent HTTP/1.1\r\n{host}\r\n"));
    assert!(
        answer.starts_with("HTTP/1.1 504 Gateway Timeout"),
        "{answer}"
    );
    let log = probe.log_text();
    assert!(log.contains("refused: Host \"evil.example:80\""), "{log}");
}

#[test]
fn streams_answers() {
    let Some((mut probe, _)) = net_probe("test.net-serve-stream", LOCAL) else {
        return;
    };
    let port = listen(&mut probe);
    let answer = exchange(
        port,
        &format!("GET /stream HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
    );
    assert!(
        answer.contains("Transfer-Encoding: chunked\r\n"),
        "{answer}"
    );
    assert!(
        answer.ends_with("\r\n\r\nb\r\ndata: one\n\n\r\nb\r\ndata: two\n\n\r\n0\r\n\r\n"),
        "{answer}"
    );

    // A client that goes away.
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .write_all(format!("GET /forever HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").as_bytes())
        .unwrap();
    let mut first = [0; 512];
    let read = stream.read(&mut first).unwrap();
    assert!(String::from_utf8_lossy(&first[..read]).starts_with("HTTP/1.1 200 OK"));
    drop(stream);
    assert_eq!(probe.notification(), "stream closed: forever");
}

#[test]
fn talks_websocket() {
    let Some((mut probe, _)) = net_probe("test.net-ws", LOCAL) else {
        return;
    };
    let port = listen(&mut probe);
    let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let (mut socket, response) =
        tungstenite::client(format!("ws://127.0.0.1:{port}/socket?token=1"), stream).unwrap();
    assert_eq!(response.status(), 101);
    assert_eq!(probe.notification(), "ws open /socket?token=1");
    socket.send(tungstenite::Message::text("hello")).unwrap();
    assert_eq!(
        socket.read().unwrap(),
        tungstenite::Message::text("echo: hello")
    );
    socket
        .send(tungstenite::Message::binary(vec![1, 2, 3]))
        .unwrap();
    assert_eq!(
        socket.read().unwrap(),
        tungstenite::Message::binary(vec![3, 2, 1])
    );
    // The plugin closes it.
    socket.send(tungstenite::Message::text("bye")).unwrap();
    loop {
        match socket.read() {
            Ok(tungstenite::Message::Close(_)) => {}
            Ok(other) => panic!("{other:?}"),
            Err(_) => break,
        }
    }
    assert_eq!(probe.notification(), "ws closed");

    // A client that closes it.
    let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let (mut socket, _) = tungstenite::client(format!("ws://localhost:{port}/"), stream).unwrap();
    assert_eq!(probe.notification(), "ws open /");
    socket.close(None).unwrap();
    while socket.read().is_ok() {}
    assert_eq!(probe.notification(), "ws closed");
}

#[test]
fn the_server_stops_with_the_plugin() {
    let Some((mut probe, _)) = net_probe("test.net-stop", LOCAL) else {
        return;
    };
    let port = listen(&mut probe);
    assert_eq!(probe.run("net:stop"), "stopped");
    assert!(TcpStream::connect(("127.0.0.1", port)).is_err());

    let port = listen(&mut probe);
    let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let (mut socket, _) = tungstenite::client(format!("ws://127.0.0.1:{port}/"), stream).unwrap();
    assert_eq!(probe.notification(), "ws open /");
    socket
        .get_mut()
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let stopped = Instant::now();
    drop(probe);
    // The connection ends, and nobody listens any more.
    while socket.read().is_ok() {}
    assert!(
        stopped.elapsed() < Duration::from_secs(2),
        "{:?}",
        stopped.elapsed()
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    while TcpStream::connect(("127.0.0.1", port)).is_ok() {
        assert!(Instant::now() < deadline, "still listening");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// HTTPS on the internet, checked by the system's certificates: `cargo test -p flux-plugin --
/// --ignored live_https` (needs the network).
#[test]
#[ignore = "needs the network"]
fn live_https() {
    let Some((mut probe, data)) = net_probe(
        "test.net-live",
        "project = \"none\"\nnetwork = [\"api.github.com\", \"*.badssl.com\"]",
    ) else {
        return;
    };
    ask(&data, "GET", "https://api.github.com/zen", true, None, "");
    let title = probe.run("net:fetch");
    assert!(title.starts_with("fetch: 200 "), "{title}");
    println!("{title}");
    ask(
        &data,
        "GET",
        "https://self-signed.badssl.com/",
        true,
        None,
        "",
    );
    let title = probe.run("net:fetch");
    assert!(
        title.starts_with("fetch failed: HttpError::Connection("),
        "{title}"
    );
    println!("{title}");
    println!("{}", probe.log_text());
}
