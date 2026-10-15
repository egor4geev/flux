//! Reading URLs: `https://`, `http://` (a local feed for manual checks) and `file://`.

use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ureq::Agent;
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

/// A failed read: `offline` — no network, the server wasn't found, a timeout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Failure {
    pub offline: bool,
    pub message: String,
}

impl Failure {
    fn server(message: String) -> Failure {
        Failure {
            offline: false,
            message,
        }
    }
}

/// The client: redirects followed (GitHub's downloads go to its storage), the system's
/// certificates (a company's root certificates work); statuses are looked at by the caller.
pub(crate) fn agent() -> Agent {
    Agent::config_builder()
        .user_agent(format!("Flux/{}", crate::current_version()))
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .tls_config(
            TlsConfig::builder()
                .provider(TlsProvider::Rustls)
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .into()
}

/// Reads a whole URL, up to `limit` bytes; `accept` — the `Accept` header of an `http(s)` request.
pub(crate) fn read(url: &str, limit: u64, accept: Option<&str>) -> Result<Vec<u8>, Failure> {
    if let Some(path) = url.strip_prefix("file://") {
        let path = file_path(path);
        return std::fs::read(&path)
            .map_err(|err| Failure::server(format!("{}: {err}", path.display())));
    }
    let mut response = get(url, accept)?;
    response
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(|err| describe(url, err))
}

/// Downloads a URL in pieces, telling the progress; stops when `cancel` is set.
pub(crate) fn download(
    url: &str,
    size: u64,
    limit: u64,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Vec<u8>, String> {
    const CHUNK: usize = 64 * 1024;
    let (mut reader, total): (Box<dyn Read>, u64) = if let Some(path) = url.strip_prefix("file://")
    {
        let path = file_path(path);
        let file =
            std::fs::File::open(&path).map_err(|err| format!("{}: {err}", path.display()))?;
        let total = file.metadata().map(|meta| meta.len()).unwrap_or(size);
        (Box::new(file.take(limit)), total)
    } else {
        let response = get(url, None).map_err(|failure| failure.message)?;
        let total = response
            .headers()
            .get("content-length")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(size);
        let reader = response
            .into_body()
            .into_with_config()
            .limit(limit)
            .reader();
        (Box::new(reader), total)
    };
    let mut bytes = Vec::with_capacity(total.min(limit) as usize);
    let mut buffer = vec![0; CHUNK];
    progress(0, total);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(crate::download::CANCELLED.into());
        }
        let read = reader
            .read(&mut buffer)
            .map_err(|err| format!("{url}: {err}"))?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        progress(bytes.len() as u64, total.max(bytes.len() as u64));
    }
    Ok(bytes)
}

/// A GET whose status is 2xx; another status is a [`Failure`] in plain words (GitHub's limit of
/// requests with the time it lifts).
fn get(url: &str, accept: Option<&str>) -> Result<ureq::http::Response<ureq::Body>, Failure> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(Failure::server(format!(
            "{url}: not an https://, http:// or file:// address"
        )));
    }
    let mut request = agent().get(url);
    if let Some(accept) = accept {
        request = request
            .header("Accept", accept)
            .header("X-GitHub-Api-Version", "2022-11-28");
    }
    let response = request.call().map_err(|err| describe(url, err))?;
    let status = response.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(response);
    }
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    };
    if (status == 403 || status == 429)
        && (header("x-ratelimit-remaining").as_deref() == Some("0")
            || header("retry-after").is_some())
    {
        return Err(Failure::server(rate_limited(
            header("x-ratelimit-reset").and_then(|reset| reset.parse().ok()),
            header("retry-after").and_then(|after| after.parse().ok()),
        )));
    }
    if status == 404 {
        return Err(Failure::server(format!(
            "{url}: not found (no release published yet?)"
        )));
    }
    Err(Failure::server(format!(
        "{url}: the server answered {status}"
    )))
}

/// GitHub's limit of requests in words: `reset` — when it lifts (Unix seconds), `retry_after` —
/// in how many seconds.
fn rate_limited(reset: Option<u64>, retry_after: Option<u64>) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0);
    let wait = retry_after.or_else(|| reset.map(|reset| reset.saturating_sub(now)));
    match wait {
        Some(seconds) => format!(
            "GitHub's limit of requests from this address is reached; try again in {} min",
            seconds.div_ceil(60).max(1)
        ),
        None => "GitHub's limit of requests from this address is reached; try again later".into(),
    }
}

/// A failed request in plain words.
fn describe(url: &str, err: ureq::Error) -> Failure {
    let offline = matches!(
        err,
        ureq::Error::HostNotFound
            | ureq::Error::Timeout(_)
            | ureq::Error::ConnectionFailed
            | ureq::Error::Io(_)
    );
    let message = match err {
        ureq::Error::StatusCode(status) => format!("{url}: the server answered {status}"),
        ureq::Error::HostNotFound => format!("{url}: the server wasn't found (offline?)"),
        ureq::Error::Timeout(_) => format!("{url}: timed out"),
        ureq::Error::BodyExceedsLimit(_) => format!("{url}: too large"),
        other => format!("{url}: {other}"),
    };
    Failure { offline, message }
}

/// `file:///Users/me/x` → `/Users/me/x` (percent-escapes decoded).
pub(crate) fn file_path(rest: &str) -> PathBuf {
    let path = rest.strip_prefix("localhost").unwrap_or(rest);
    PathBuf::from(percent_decode(path))
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&text[i + 1..i + 3], 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_urls() {
        assert_eq!(
            file_path("/Users/me/My%20Folder/x.zip"),
            PathBuf::from("/Users/me/My Folder/x.zip")
        );
        assert_eq!(file_path("localhost/tmp/a"), PathBuf::from("/tmp/a"));
    }

    #[test]
    fn limits_in_words() {
        assert!(rate_limited(None, Some(30)).contains("1 min"));
        assert!(rate_limited(None, Some(600)).contains("10 min"));
        assert!(rate_limited(None, None).contains("later"));
    }

    #[test]
    fn not_a_url() {
        let failure = read("ftp://x/y", 10, None).unwrap_err();
        assert!(!failure.offline);
        assert!(failure.message.contains("not an https://"));
    }
}
