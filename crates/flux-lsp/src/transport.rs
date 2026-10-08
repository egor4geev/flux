//! JSON-RPC over stdio: `Content-Length` framing, reading and writing on background threads.

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Write};
use std::process::{ChildStderr, ChildStdin};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread;

/// How many last lines of the server's stderr are kept for the exit reason.
const STDERR_LINES: usize = 20;

/// A message with its header, ready to be written.
pub(crate) fn frame(body: &[u8]) -> Vec<u8> {
    let mut message = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    message.extend_from_slice(body);
    message
}

/// The next message body; `None` at the end of the stream between messages. Header lines other
/// than `Content-Length` are ignored, and so is anything a server prints that doesn't look like a
/// header (some print a log line to stdout before speaking the protocol).
pub(crate) fn read_message(reader: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut length = None;
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return if length.is_none() {
                Ok(None)
            } else {
                Err(io::ErrorKind::UnexpectedEof.into())
            };
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end_matches(['\r', '\n']);
        if text.is_empty() {
            if let Some(length) = length {
                let mut body = vec![0; length];
                reader.read_exact(&mut body)?;
                return Ok(Some(body));
            }
            continue;
        }
        if let Some((name, value)) = text.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().ok();
        }
    }
}

/// Writes framed messages to the server's stdin in order until the channel closes or the pipe
/// breaks. A server that doesn't read blocks only this thread.
pub(crate) fn spawn_writer(name: &str, stdin: ChildStdin, messages: Receiver<Vec<u8>>) {
    let spawned = thread::Builder::new()
        .name(format!("lsp-{name}-writer"))
        .spawn(move || {
            let mut stdin = stdin;
            for message in messages {
                if stdin
                    .write_all(&message)
                    .and_then(|()| stdin.flush())
                    .is_err()
                {
                    break;
                }
            }
        });
    if let Err(err) = spawned {
        eprintln!("flux-lsp: can't start a writer thread: {err}");
    }
}

/// The server's stderr: the last lines are kept for the exit reason (and logged with
/// `FLUX_LSP_LOG`); `closed` is set at the end of the stream. Reading keeps the pipe from filling up
/// and blocking the server.
pub(crate) struct Stderr {
    pub lines: Mutex<VecDeque<String>>,
    pub closed: AtomicBool,
}

pub(crate) fn spawn_stderr(name: &str, stderr: ChildStderr, log: bool) -> Arc<Stderr> {
    let sink = Arc::new(Stderr {
        lines: Mutex::new(VecDeque::new()),
        closed: AtomicBool::new(false),
    });
    let shared = sink.clone();
    let server = name.to_string();
    let spawned = thread::Builder::new()
        .name(format!("lsp-{name}-stderr"))
        .spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut line = Vec::new();
            while let Ok(read) = reader.read_until(b'\n', &mut line) {
                if read == 0 {
                    break;
                }
                let text = String::from_utf8_lossy(&line).trim_end().to_string();
                line.clear();
                if log {
                    eprintln!("[lsp {server}] stderr: {text}");
                }
                if text.is_empty() {
                    continue;
                }
                let mut lines = shared.lines.lock().unwrap_or_else(|e| e.into_inner());
                if lines.len() == STDERR_LINES {
                    lines.pop_front();
                }
                lines.push_back(text);
            }
            shared.closed.store(true, Ordering::Release);
        });
    if spawned.is_err() {
        sink.closed.store(true, Ordering::Release);
    }
    sink
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let mut stream = frame(br#"{"a":1}"#);
        stream.extend(frame("{\"b\":\"Я\"}".as_bytes()));
        let mut reader = io::Cursor::new(stream);
        assert_eq!(read_message(&mut reader).unwrap().unwrap(), br#"{"a":1}"#);
        assert_eq!(
            read_message(&mut reader).unwrap().unwrap(),
            "{\"b\":\"Я\"}".as_bytes()
        );
        assert_eq!(read_message(&mut reader).unwrap(), None);
    }

    #[test]
    fn other_headers_and_noise_are_skipped() {
        let stream = b"Starting server...\nContent-Type: application/vscode-jsonrpc; charset=utf-8\r\ncontent-length: 2\r\n\r\n{}";
        let mut reader = io::Cursor::new(&stream[..]);
        assert_eq!(read_message(&mut reader).unwrap().unwrap(), b"{}");
    }

    #[test]
    fn truncated_message_is_an_error() {
        let mut reader = io::Cursor::new(&b"Content-Length: 10\r\n\r\n{}"[..]);
        assert!(read_message(&mut reader).is_err());
        let mut reader = io::Cursor::new(&b"Content-Length: 10\r\n"[..]);
        assert!(read_message(&mut reader).is_err());
    }
}
