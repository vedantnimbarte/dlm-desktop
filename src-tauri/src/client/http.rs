//! A minimal HTTP/1.1 client for talking to a `dlm serve` instance.
//!
//! There is no HTTP crate in this project's dependency tree on purpose. dlm's
//! server is itself a dependency-free HTTP/1.1 implementation
//! (`dlm/src/server/http.rs`) that speaks plain TCP with no TLS, so the surface
//! we must cover is small and fully known:
//!
//! - Requests **must** carry `Content-Length`; chunked bodies are refused with
//!   `411` (`dlm/src/server/http.rs:435`).
//! - Buffered responses carry `Content-Length` and may keep the connection
//!   alive.
//! - Streaming (SSE) responses carry **no** `Content-Length` and always set
//!   `Connection: close` (`dlm/src/server/http.rs:556`), so the body runs to
//!   EOF. That is what makes the streaming path here simple: read until the
//!   peer hangs up.
//!
//! Everything is blocking and runs on a caller-owned thread. Cancellation works
//! by way of a short socket read timeout plus a shared flag, so a user hitting
//! "stop" mid-generation takes effect within one timeout tick rather than
//! waiting on the next token.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// How long a blocking read parks before we re-check the cancel flag. Also
/// bounds how long "stop" takes to be honoured.
const READ_TICK: Duration = Duration::from_millis(200);

/// Connect timeout for a single attempt. The supervisor retries while a
/// freshly-spawned server is still binding its port.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Refuse absurd header blocks rather than growing a buffer without bound.
const MAX_HEADER_BYTES: usize = 64 * 1024;

/// Cap on a buffered (non-streaming) response body. Generous for `/v1/models`
/// and a completion, and far below anything that would exhaust memory.
const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("connect to {addr}: {source}")]
    Connect {
        addr: String,
        #[source]
        source: std::io::Error,
    },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("malformed response: {0}")]
    Protocol(String),
    /// A non-2xx status. `body` is retained because dlm returns a JSON error
    /// envelope the UI shows verbatim.
    #[error("http {status}: {body}")]
    Status { status: u16, body: String },
    /// The caller's cancel flag was set; not really an error, but it has to
    /// unwind the read loop.
    #[error("cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, HttpError>;

/// Where a dlm server lives, and how to authenticate to it.
///
/// The same type covers a locally-spawned sidecar and a remote attach — the
/// only difference is who owns the process, which is the supervisor's problem,
/// not this module's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    /// Sent as both `Authorization: Bearer` and `x-api-key` so either dialect
    /// authenticates; dlm accepts either form on every guarded route.
    pub api_key: Option<String>,
}

impl Endpoint {
    pub fn local(port: u16) -> Self {
        Self {
            host: "127.0.0.1".into(),
            port,
            api_key: None,
        }
    }

    /// Parse `host:port`, `http://host:port`, or `http://host` (port 80).
    ///
    /// `https://` is rejected rather than silently downgraded: dlm serves no
    /// TLS, so accepting the scheme would promise transport security the
    /// connection does not have.
    pub fn parse(input: &str) -> std::result::Result<Self, String> {
        let s = input.trim();
        if s.is_empty() {
            return Err("empty address".into());
        }
        if let Some(rest) = s.strip_prefix("https://") {
            let _ = rest;
            return Err("dlm serves plain HTTP; https:// is not supported".into());
        }
        let s = s.strip_prefix("http://").unwrap_or(s);
        let s = s.split('/').next().unwrap_or(s); // drop any path
        let (host, port) = match s.rsplit_once(':') {
            Some((h, p)) => {
                let port: u16 = p.parse().map_err(|_| format!("bad port: {p}"))?;
                (h, port)
            }
            None => (s, 80),
        };
        if host.is_empty() {
            return Err("empty host".into());
        }
        Ok(Self {
            host: host.to_string(),
            port,
            api_key: None,
        })
    }

    pub fn with_api_key(mut self, key: Option<String>) -> Self {
        self.api_key = key.filter(|k| !k.is_empty());
        self
    }

    pub fn authority(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    pub fn base_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }

    fn connect(&self) -> Result<TcpStream> {
        let authority = self.authority();
        let addr = authority
            .to_socket_addrs()
            .map_err(|source| HttpError::Connect {
                addr: authority.clone(),
                source,
            })?
            .next()
            .ok_or_else(|| HttpError::Connect {
                addr: authority.clone(),
                source: std::io::Error::new(
                    std::io::ErrorKind::AddrNotAvailable,
                    "no address resolved",
                ),
            })?;
        let sock =
            TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).map_err(|source| {
                HttpError::Connect {
                    addr: authority,
                    source,
                }
            })?;
        // Token-at-a-time SSE frames are small; Nagle would batch them into
        // latency the user reads as stutter.
        let _ = sock.set_nodelay(true);
        Ok(sock)
    }

    fn write_request(
        &self,
        sock: &mut TcpStream,
        method: &str,
        path: &str,
        body: Option<&[u8]>,
        accept: &str,
    ) -> Result<()> {
        let mut head = String::with_capacity(256);
        head.push_str(&format!("{method} {path} HTTP/1.1\r\n"));
        head.push_str(&format!("Host: {}\r\n", self.authority()));
        head.push_str("User-Agent: dlm-desktop\r\n");
        head.push_str(&format!("Accept: {accept}\r\n"));
        if let Some(key) = &self.api_key {
            head.push_str(&format!("Authorization: Bearer {key}\r\n"));
            head.push_str(&format!("x-api-key: {key}\r\n"));
        }
        match body {
            Some(b) => {
                head.push_str("Content-Type: application/json\r\n");
                head.push_str(&format!("Content-Length: {}\r\n", b.len()));
            }
            // dlm's parser refuses chunked, so a bodyless request states zero
            // explicitly rather than omitting the header.
            None if method != "GET" => head.push_str("Content-Length: 0\r\n"),
            None => {}
        }
        head.push_str("\r\n");
        sock.write_all(head.as_bytes())?;
        if let Some(b) = body {
            sock.write_all(b)?;
        }
        sock.flush()?;
        Ok(())
    }
}

/// A fully-buffered response.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// Turn a non-2xx into an error, keeping the body for the UI to show.
    pub fn ok_or_status(self) -> Result<Self> {
        if (200..300).contains(&self.status) {
            Ok(self)
        } else {
            Err(HttpError::Status {
                status: self.status,
                body: self.text(),
            })
        }
    }
}

/// One parsed Server-Sent Event.
///
/// dlm's OpenAI dialect emits bare `data:` frames; the Anthropic dialect and
/// the telemetry stream name their frames with `event:`
/// (`dlm/src/server/engine.rs:1398`).
#[derive(Debug, Clone, Default)]
pub struct SseFrame {
    pub event: Option<String>,
    pub data: String,
}

impl SseFrame {
    /// OpenAI's end-of-stream sentinel.
    pub fn is_done(&self) -> bool {
        self.data.trim() == "[DONE]"
    }
}

/// What the SSE callback wants to happen next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Stop,
}

/// Shared cancellation token. Cloned into the UI so "stop" can flip it.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Perform a buffered request and read the whole response.
pub fn request(
    endpoint: &Endpoint,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
) -> Result<Response> {
    let mut sock = endpoint.connect()?;
    // A buffered call should not hang forever if the engine wedges mid-token;
    // it is generous because a cold model load can precede the first response.
    sock.set_read_timeout(Some(Duration::from_secs(120)))?;
    endpoint.write_request(&mut sock, method, path, body, "application/json")?;

    let mut reader = BufReader::new(sock);
    let (status, headers) = read_head(&mut reader)?;

    let len = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok());

    let body = match len {
        Some(n) if n > MAX_BODY_BYTES => {
            return Err(HttpError::Protocol(format!("response body too large: {n}")))
        }
        Some(n) => {
            let mut buf = vec![0u8; n];
            reader.read_exact(&mut buf)?;
            buf
        }
        // No Content-Length on a buffered route means `Connection: close`
        // framing; read to EOF, still capped.
        None => {
            let mut buf = Vec::new();
            reader.take(MAX_BODY_BYTES as u64).read_to_end(&mut buf)?;
            buf
        }
    };

    Ok(Response {
        status,
        headers,
        body,
    })
}

/// Open an SSE stream and drive `on_frame` until the server closes, the
/// callback returns [`Flow::Stop`], or `cancel` is flipped.
///
/// Returns `Ok(())` on a clean end-of-stream. Cancellation returns
/// `Err(HttpError::Cancelled)` so callers can distinguish "user stopped" from
/// "model finished".
pub fn stream_sse(
    endpoint: &Endpoint,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    cancel: &Cancel,
    mut on_frame: impl FnMut(SseFrame) -> Flow,
) -> Result<()> {
    let mut sock = endpoint.connect()?;
    // Short tick so cancellation is responsive; WouldBlock is expected and
    // simply means "no bytes yet, check the flag and go round again".
    sock.set_read_timeout(Some(READ_TICK))?;
    endpoint.write_request(&mut sock, method, path, body, "text/event-stream")?;

    let mut reader = BufReader::new(sock);
    let (status, headers) = read_head_cancellable(&mut reader, cancel)?;
    if !(200..300).contains(&status) {
        // An error on a streaming route is a normal buffered JSON body.
        let mut buf = Vec::new();
        let _ = reader.take(MAX_BODY_BYTES as u64).read_to_end(&mut buf);
        return Err(HttpError::Status {
            status,
            body: String::from_utf8_lossy(&buf).into_owned(),
        });
    }
    let ctype = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.to_ascii_lowercase())
        .unwrap_or_default();
    if !ctype.contains("text/event-stream") {
        return Err(HttpError::Protocol(format!(
            "expected text/event-stream, got {ctype:?}"
        )));
    }

    // Accumulate raw bytes and split on a blank line. dlm writes "\n\n"
    // terminators; the spec permits "\r\n\r\n", so both are handled.
    let mut pending = Vec::<u8>::new();
    let mut chunk = [0u8; 8192];
    loop {
        if cancel.is_cancelled() {
            return Err(HttpError::Cancelled);
        }
        match reader.read(&mut chunk) {
            Ok(0) => break, // peer closed: clean end of stream
            Ok(n) => {
                pending.extend_from_slice(&chunk[..n]);
                while let Some((frame_bytes, rest)) = split_frame(&pending) {
                    let frame = parse_frame(&frame_bytes);
                    pending = rest;
                    if let Some(frame) = frame {
                        if on_frame(frame) == Flow::Stop {
                            return Ok(());
                        }
                    }
                }
                if pending.len() > MAX_HEADER_BYTES {
                    return Err(HttpError::Protocol("unterminated SSE frame".into()));
                }
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// Split off the first complete SSE frame, returning (frame, remainder).
fn split_frame(buf: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut i = 0;
    while i + 1 < buf.len() {
        if buf[i] == b'\n' && buf[i + 1] == b'\n' {
            return Some((buf[..i].to_vec(), buf[i + 2..].to_vec()));
        }
        if i + 3 < buf.len() && &buf[i..i + 4] == b"\r\n\r\n" {
            return Some((buf[..i].to_vec(), buf[i + 4..].to_vec()));
        }
        i += 1;
    }
    None
}

/// Parse one frame's lines. Returns `None` for a comment-only or empty frame
/// (SSE keep-alives are `:`-prefixed and carry no data).
fn parse_frame(bytes: &[u8]) -> Option<SseFrame> {
    let text = String::from_utf8_lossy(bytes);
    let mut frame = SseFrame::default();
    let mut data_lines: Vec<&str> = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(v) = line.strip_prefix("event:") {
            frame.event = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("data:") {
            // Exactly one leading space is part of the framing, not the data.
            data_lines.push(v.strip_prefix(' ').unwrap_or(v));
        }
    }
    if data_lines.is_empty() && frame.event.is_none() {
        return None;
    }
    frame.data = data_lines.join("\n");
    Some(frame)
}

/// Read the status line + headers.
fn read_head(reader: &mut BufReader<TcpStream>) -> Result<(u16, Vec<(String, String)>)> {
    read_head_inner(reader, None)
}

fn read_head_cancellable(
    reader: &mut BufReader<TcpStream>,
    cancel: &Cancel,
) -> Result<(u16, Vec<(String, String)>)> {
    read_head_inner(reader, Some(cancel))
}

fn read_head_inner(
    reader: &mut BufReader<TcpStream>,
    cancel: Option<&Cancel>,
) -> Result<(u16, Vec<(String, String)>)> {
    let mut line = String::new();
    read_line(reader, &mut line, cancel)?;
    let status = parse_status_line(&line)?;

    let mut headers = Vec::new();
    let mut total = line.len();
    loop {
        line.clear();
        read_line(reader, &mut line, cancel)?;
        total += line.len();
        if total > MAX_HEADER_BYTES {
            return Err(HttpError::Protocol("header block too large".into()));
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    Ok((status, headers))
}

/// `read_line` that tolerates the short read timeout used for cancellation.
fn read_line(
    reader: &mut BufReader<TcpStream>,
    out: &mut String,
    cancel: Option<&Cancel>,
) -> Result<()> {
    loop {
        if let Some(c) = cancel {
            if c.is_cancelled() {
                return Err(HttpError::Cancelled);
            }
        }
        match reader.read_line(out) {
            Ok(0) => {
                return Err(HttpError::Protocol(
                    "connection closed before response head".into(),
                ))
            }
            Ok(_) => return Ok(()),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                // Only safe to retry while nothing has been consumed into
                // `out`; BufReader keeps partial data in its buffer, and a
                // subsequent read_line appends, so this stays correct.
                continue;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

fn parse_status_line(line: &str) -> Result<u16> {
    let mut parts = line.split_whitespace();
    let version = parts
        .next()
        .ok_or_else(|| HttpError::Protocol("empty status line".into()))?;
    if !version.starts_with("HTTP/") {
        return Err(HttpError::Protocol(format!("not HTTP: {version:?}")));
    }
    let code = parts
        .next()
        .ok_or_else(|| HttpError::Protocol("status line has no code".into()))?;
    code.parse::<u16>()
        .map_err(|_| HttpError::Protocol(format!("bad status code: {code:?}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_parses_the_forms_a_user_might_type() {
        assert_eq!(
            Endpoint::parse("localhost:8000").unwrap(),
            Endpoint {
                host: "localhost".into(),
                port: 8000,
                api_key: None
            }
        );
        assert_eq!(
            Endpoint::parse("http://10.0.0.5:8080/").unwrap(),
            Endpoint {
                host: "10.0.0.5".into(),
                port: 8080,
                api_key: None
            }
        );
        assert_eq!(Endpoint::parse("example.com").unwrap().port, 80);
    }

    /// dlm serves no TLS, so accepting `https://` would misrepresent the
    /// connection rather than merely failing to connect.
    #[test]
    fn https_is_refused_not_downgraded() {
        assert!(Endpoint::parse("https://host:8000").is_err());
    }

    #[test]
    fn empty_or_bad_port_is_an_error() {
        assert!(Endpoint::parse("").is_err());
        assert!(Endpoint::parse("host:notaport").is_err());
        assert!(Endpoint::parse(":8000").is_err());
    }

    #[test]
    fn api_key_is_dropped_when_blank() {
        let e = Endpoint::local(8000).with_api_key(Some(String::new()));
        assert_eq!(e.api_key, None);
    }

    #[test]
    fn frames_split_on_lf_lf_and_crlf_crlf() {
        let buf = b"data: a\n\ndata: b\r\n\r\ndata: c";
        let (f1, rest) = split_frame(buf).unwrap();
        assert_eq!(f1, b"data: a");
        let (f2, rest) = split_frame(&rest).unwrap();
        assert_eq!(f2, b"data: b");
        // A trailing partial frame stays pending rather than being emitted.
        assert!(split_frame(&rest).is_none());
    }

    #[test]
    fn parses_openai_and_anthropic_shapes() {
        let f = parse_frame(b"data: {\"x\":1}").unwrap();
        assert_eq!(f.event, None);
        assert_eq!(f.data, "{\"x\":1}");

        let f = parse_frame(b"event: content_block_delta\ndata: {\"y\":2}").unwrap();
        assert_eq!(f.event.as_deref(), Some("content_block_delta"));
        assert_eq!(f.data, "{\"y\":2}");
    }

    #[test]
    fn done_sentinel_is_recognised() {
        assert!(parse_frame(b"data: [DONE]").unwrap().is_done());
        assert!(!parse_frame(b"data: {}").unwrap().is_done());
    }

    /// Keep-alive comments must not surface as empty assistant tokens.
    #[test]
    fn comment_frames_are_skipped() {
        assert!(parse_frame(b": keep-alive").is_none());
        assert!(parse_frame(b"").is_none());
    }

    #[test]
    fn multi_line_data_is_joined_with_newlines() {
        let f = parse_frame(b"data: line one\ndata: line two").unwrap();
        assert_eq!(f.data, "line one\nline two");
    }

    #[test]
    fn status_line_parsing() {
        assert_eq!(parse_status_line("HTTP/1.1 200 OK\r\n").unwrap(), 200);
        assert_eq!(parse_status_line("HTTP/1.0 404 Not Found\r\n").unwrap(), 404);
        assert!(parse_status_line("garbage\r\n").is_err());
    }
}
