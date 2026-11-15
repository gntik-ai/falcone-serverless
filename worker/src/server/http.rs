//! The smallest HTTP/1.1 the two listeners need: one request per connection, a request line,
//! headers and a `Content-Length` body, and a JSON response. Everything is bounded: header
//! bytes, body bytes, the time to receive the headers, the time to receive the body, the idle
//! time between reads, and the time to write the response. Every deadline is absolute from the
//! start of its phase, so a peer that drips bytes cannot extend it.

use crate::api::{Code, Reason, Refusal};
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

/// The most header bytes, request line included.
pub const MAX_HEADERS: usize = 16 * 1024;
/// The most body bytes of an execute request.
pub const MAX_REQUEST: usize = 8 * 1024 * 1024;
/// The most body bytes of a control request (cancel, update).
pub const MAX_CONTROL: usize = 64 * 1024;

/// The deadlines of one listener, fixed per listener.
#[derive(Clone, Copy, Debug)]
pub struct Deadlines {
    /// The whole time allowed to receive the request line and headers.
    pub header: Duration,
    /// The whole time allowed to receive the body.
    pub body: Duration,
    /// The longest a read may wait for the next byte.
    pub idle: Duration,
    /// The whole time allowed to write the response.
    pub response: Duration,
}

impl Deadlines {
    /// The execute listener's documented deadlines.
    pub const EXECUTE: Self = Self {
        header: Duration::from_secs(5),
        body: Duration::from_secs(30),
        idle: Duration::from_secs(5),
        response: Duration::from_secs(5),
    };
    /// The control listener's documented deadlines: its requests are small.
    pub const CONTROL: Self = Self {
        header: Duration::from_secs(2),
        body: Duration::from_secs(2),
        idle: Duration::from_secs(1),
        response: Duration::from_secs(2),
    };
}

/// A request line and headers, with the body bytes that arrived with them.
pub struct Head {
    pub method: String,
    pub path: String,
    pub content_length: Option<usize>,
    /// Body bytes received together with the headers.
    pub buffered: Vec<u8>,
}

/// Why a request could not be read; each maps to a documented refusal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Unreadable {
    /// Not HTTP, or an unsupported framing (chunked, a bad length).
    Malformed,
    HeadersTooLarge,
    BodyTooLarge,
    /// A deadline passed or the peer went away; the connection is closed with no answer.
    Closed,
}

impl Unreadable {
    pub fn refusal(self) -> Option<Refusal> {
        match self {
            Self::Malformed => Some(Refusal::new(Code::InvalidRequest, Reason::MalformedHttp)),
            Self::HeadersTooLarge => {
                Some(Refusal::new(Code::RequestTooLarge, Reason::HeadersTooLarge))
            }
            Self::BodyTooLarge => Some(Refusal::new(Code::RequestTooLarge, Reason::BodyTooLarge)),
            Self::Closed => None,
        }
    }
}

fn read_some(
    stream: &mut TcpStream,
    buf: &mut [u8],
    limit: Instant,
    idle: Duration,
) -> io::Result<usize> {
    let remaining = limit.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(io::ErrorKind::TimedOut.into());
    }
    stream.set_read_timeout(Some(remaining.min(idle)))?;
    loop {
        match stream.read(buf) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            other => return other,
        }
    }
}

/// Reads the request line and headers within `deadlines.header`.
pub fn read_head(stream: &mut TcpStream, deadlines: Deadlines) -> Result<Head, Unreadable> {
    let limit = Instant::now() + deadlines.header;
    let mut data: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    let end = loop {
        if let Some(at) = find(&data, b"\r\n\r\n") {
            break at;
        }
        if data.len() > MAX_HEADERS {
            return Err(Unreadable::HeadersTooLarge);
        }
        match read_some(stream, &mut chunk, limit, deadlines.idle) {
            Ok(0) | Err(_) => return Err(Unreadable::Closed),
            Ok(count) => data.extend_from_slice(&chunk[..count]),
        }
    };
    if end + 4 > MAX_HEADERS {
        return Err(Unreadable::HeadersTooLarge);
    }
    let text = std::str::from_utf8(&data[..end]).map_err(|_| Unreadable::Malformed)?;
    let mut lines = text.split("\r\n");
    let mut request_line = lines.next().unwrap_or("").split(' ');
    let (Some(method), Some(path), Some(version), None) = (
        request_line.next(),
        request_line.next(),
        request_line.next(),
        request_line.next(),
    ) else {
        return Err(Unreadable::Malformed);
    };
    if !version.starts_with("HTTP/1.") || method.is_empty() || !path.starts_with('/') {
        return Err(Unreadable::Malformed);
    }
    let mut content_length = None;
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(Unreadable::Malformed)?;
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(Unreadable::Malformed);
        }
        if name.eq_ignore_ascii_case("content-length") {
            let value = value.trim();
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(Unreadable::Malformed);
            }
            let length: usize = value.parse().map_err(|_| Unreadable::BodyTooLarge)?;
            if content_length.is_some_and(|seen| seen != length) {
                return Err(Unreadable::Malformed);
            }
            content_length = Some(length);
        }
    }
    Ok(Head {
        method: method.to_owned(),
        path: path.to_owned(),
        content_length,
        buffered: data[end + 4..].to_vec(),
    })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Reads the rest of a body of `length` bytes within `deadlines.body`, with at most
/// `deadlines.idle` between reads. `head.buffered` holds the part already received.
pub fn read_body(
    stream: &mut TcpStream,
    head: &mut Head,
    length: usize,
    deadlines: Deadlines,
) -> Result<Vec<u8>, Unreadable> {
    let limit = Instant::now() + deadlines.body;
    let mut body = std::mem::take(&mut head.buffered);
    if body.len() > length {
        body.truncate(length);
    }
    let mut chunk = vec![0u8; 64 * 1024];
    while body.len() < length {
        let want = (length - body.len()).min(chunk.len());
        match read_some(stream, &mut chunk[..want], limit, deadlines.idle) {
            Ok(0) | Err(_) => return Err(Unreadable::Closed),
            Ok(count) => body.extend_from_slice(&chunk[..count]),
        }
    }
    Ok(body)
}

/// Takes the body bytes that are already available without waiting for any.
pub fn drain_available(stream: &mut TcpStream, head: &mut Head, length: usize) {
    if head.buffered.len() >= length || stream.set_nonblocking(true).is_err() {
        return;
    }
    let mut chunk = vec![0u8; 64 * 1024];
    while head.buffered.len() < length {
        let want = (length - head.buffered.len()).min(chunk.len());
        match stream.read(&mut chunk[..want]) {
            Ok(0) | Err(_) => break,
            Ok(count) => head.buffered.extend_from_slice(&chunk[..count]),
        }
    }
    let _ = stream.set_nonblocking(false);
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        404 => "Not Found",
        409 => "Conflict",
        413 => "Payload Too Large",
        422 => "Unprocessable Entity",
        499 => "Client Closed Request",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Error",
    }
}

/// Writes the whole response within `deadline` from now, then closes. One absolute deadline
/// covers every partial write, so a peer that reads a byte at a time is cut off.
pub fn respond(
    stream: &mut TcpStream,
    status: u16,
    body: &[u8],
    deadline: Duration,
) -> io::Result<()> {
    write_response(stream, status, body, deadline)?;
    linger(stream);
    Ok(())
}

/// Answers a connection the listener has no capacity for: one short write, no waiting.
pub fn respond_now(stream: &mut TcpStream, status: u16, body: &[u8]) -> io::Result<()> {
    write_response(stream, status, body, Duration::from_millis(50))
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    body: &[u8],
    deadline: Duration,
) -> io::Result<()> {
    let limit = Instant::now() + deadline;
    let mut response = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        reason(status),
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body);
    let mut written = 0;
    while written < response.len() {
        let remaining = limit.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        stream.set_write_timeout(Some(remaining))?;
        match stream.write(&response[written..]) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(count) => written += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    let _ = stream.shutdown(std::net::Shutdown::Write);
    Ok(())
}

/// Discards what the peer already sent, for a moment and within a small bound, so that closing
/// with unread bytes does not reset the connection before the peer has read the response.
fn linger(stream: &mut TcpStream) {
    let limit = Instant::now() + Duration::from_millis(100);
    let mut chunk = [0u8; 8192];
    let mut discarded = 0;
    while discarded < 256 * 1024 {
        let remaining = limit.saturating_duration_since(Instant::now());
        if remaining.is_zero() || stream.set_read_timeout(Some(remaining)).is_err() {
            break;
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(count) => discarded += count,
        }
    }
}

/// Answers with a bare refusal envelope (`status`, `code`, `message`, `detail`, `retryable`).
pub fn refuse(stream: &mut TcpStream, refusal: &Refusal, deadline: Duration) {
    let body = serde_json::to_vec(refusal).unwrap_or_default();
    let _ = respond(stream, refusal.status(), &body, deadline);
}
