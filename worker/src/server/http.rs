//! Bounded HTTP/1.1 for the execute and control listeners. One request per connection, always
//! `Connection: close`, no chunked bodies, no keep-alive, no pipelining.
//!
//! Every phase has a documented bound, enforced on the socket (never on a buffer):
//!
//! | Phase | Bound |
//! | --- | --- |
//! | request line and headers | [`MAX_HEADERS`] bytes, one absolute [`Deadlines::header`] |
//! | body | the listener's limit (at most [`MAX_REQUEST`]), [`Deadlines::io`] of inactivity, one absolute [`Deadlines::body`] |
//! | response | one absolute [`Deadlines::response`] across every write |

use std::io::{self, ErrorKind, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::time::{Duration, Instant};

/// The most request-line-and-header bytes accepted.
pub const MAX_HEADERS: usize = 16 * 1024;
/// The largest request body, execute listener.
pub const MAX_REQUEST: usize = 8 * 1024 * 1024;
/// The largest request body, control listener.
pub const MAX_CONTROL_BODY: usize = 16 * 1024;
/// The longest request path.
const MAX_PATH: usize = 256;
/// What is read and discarded after an early refusal, so the client can read the refusal
/// instead of a connection reset.
const DRAIN_BYTES: usize = 1024 * 1024;
const DRAIN_DEADLINE: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug)]
pub struct Deadlines {
    /// All of the request line and headers.
    pub header: Duration,
    /// Silence tolerated while the body is being read.
    pub io: Duration,
    /// All of the body.
    pub body: Duration,
    /// All of the response, across every write.
    pub response: Duration,
}

impl Default for Deadlines {
    fn default() -> Self {
        Self {
            header: Duration::from_secs(5),
            io: Duration::from_secs(5),
            body: Duration::from_secs(30),
            response: Duration::from_secs(5),
        }
    }
}

pub struct Head {
    pub method: String,
    pub path: String,
    pub content_length: usize,
    leftover: Vec<u8>,
}

/// Why a request was not read in full.
#[derive(Debug, PartialEq, Eq)]
pub enum Failure {
    /// Not HTTP the worker accepts: answered 400.
    Malformed,
    /// More than [`MAX_HEADERS`]: answered 413 before any further read.
    HeadersTooLarge,
    /// A `Content-Length` above the listener's limit: answered 413 before the body is read.
    BodyTooLarge,
    /// The peer is gone or too slow: the connection is closed without an answer. The label
    /// names the bound that ended it.
    Closed(&'static str),
}

fn timed_out(error: &io::Error) -> bool {
    matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
}

fn end_of_headers(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|at| at + 4)
}

/// Reads the request line and headers within [`MAX_HEADERS`] and `deadlines.header`.
pub fn read_head(
    stream: &mut TcpStream,
    deadlines: &Deadlines,
    max_body: usize,
) -> Result<Head, Failure> {
    let deadline = Instant::now() + deadlines.header;
    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];
    let end = loop {
        if let Some(end) = end_of_headers(&buffer) {
            break end;
        }
        if buffer.len() > MAX_HEADERS {
            return Err(Failure::HeadersTooLarge);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Failure::Closed("header_deadline"));
        }
        let _ = stream.set_read_timeout(Some(remaining));
        match stream.read(&mut chunk) {
            Ok(0) => return Err(Failure::Closed("peer_closed")),
            Ok(count) => buffer.extend_from_slice(&chunk[..count]),
            Err(error) if timed_out(&error) || error.kind() == ErrorKind::Interrupted => {}
            Err(_) => return Err(Failure::Closed("read_error")),
        }
    };
    if end > MAX_HEADERS {
        return Err(Failure::HeadersTooLarge);
    }
    let mut head = parse_head(&buffer[..end - 4], max_body)?;
    head.leftover = buffer[end..].to_vec();
    Ok(head)
}

fn parse_head(text: &[u8], max_body: usize) -> Result<Head, Failure> {
    let text = std::str::from_utf8(text).map_err(|_| Failure::Malformed)?;
    let mut lines = text.split("\r\n");
    let mut request_line = lines.next().ok_or(Failure::Malformed)?.split(' ');
    let (Some(method), Some(path), Some(version), None) = (
        request_line.next(),
        request_line.next(),
        request_line.next(),
        request_line.next(),
    ) else {
        return Err(Failure::Malformed);
    };
    if !matches!(version, "HTTP/1.1" | "HTTP/1.0")
        || method.is_empty()
        || !method.bytes().all(|byte| byte.is_ascii_uppercase())
        || path.is_empty()
        || path.len() > MAX_PATH
        || !path.starts_with('/')
        || !path.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(Failure::Malformed);
    }
    let mut content_length = None;
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(Failure::Malformed)?;
        if name.is_empty() || !name.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err(Failure::Malformed);
        }
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(Failure::Malformed);
        }
        if name.eq_ignore_ascii_case("content-length") {
            let value = value.trim();
            if content_length.is_some()
                || value.is_empty()
                || value.len() > 19
                || !value.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Err(Failure::Malformed);
            }
            content_length = value.parse::<u64>().ok();
        }
    }
    let content_length = content_length.ok_or(Failure::Malformed)?;
    if content_length > max_body as u64 {
        return Err(Failure::BodyTooLarge);
    }
    Ok(Head {
        method: method.to_owned(),
        path: path.to_owned(),
        content_length: content_length as usize,
        leftover: Vec::new(),
    })
}

/// Reads the body announced by `head`: silence is bounded by `deadlines.io`, the whole by
/// `deadlines.body`.
pub fn read_body(
    stream: &mut TcpStream,
    head: &mut Head,
    deadlines: &Deadlines,
) -> Result<Vec<u8>, Failure> {
    let wanted = head.content_length;
    let mut body = std::mem::take(&mut head.leftover);
    body.truncate(wanted);
    let deadline = Instant::now() + deadlines.body;
    let mut chunk = vec![0u8; 64 * 1024];
    while body.len() < wanted {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Failure::Closed("body_deadline"));
        }
        let _ = stream.set_read_timeout(Some(remaining.min(deadlines.io)));
        let limit = chunk.len().min(wanted - body.len());
        match stream.read(&mut chunk[..limit]) {
            Ok(0) => return Err(Failure::Closed("peer_closed")),
            Ok(count) => body.extend_from_slice(&chunk[..count]),
            Err(error) if timed_out(&error) => {
                return Err(Failure::Closed(if Instant::now() >= deadline {
                    "body_deadline"
                } else {
                    "body_inactivity"
                }));
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(_) => return Err(Failure::Closed("read_error")),
        }
    }
    Ok(body)
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Content Too Large",
        422 => "Unprocessable Content",
        499 => "Client Closed Request",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Unknown",
    }
}

/// Why a response was not delivered.
#[derive(Debug, PartialEq, Eq)]
pub enum WriteFailure {
    /// The absolute response deadline passed; the connection is closed at once.
    Deadline,
    Io,
}

/// Writes one response under one absolute deadline across every write: a reader that takes a
/// byte at a time is cut off when the deadline passes, never granted a fresh timeout per write.
pub fn write_response(
    stream: &mut TcpStream,
    status: u16,
    body: &[u8],
    deadline: Duration,
) -> Result<(), WriteFailure> {
    let mut bytes = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        reason_phrase(status),
        body.len()
    )
    .into_bytes();
    bytes.extend_from_slice(body);
    let deadline = Instant::now() + deadline;
    let mut written = 0;
    while written < bytes.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let _ = stream.shutdown(Shutdown::Both);
            return Err(WriteFailure::Deadline);
        }
        let _ = stream.set_write_timeout(Some(remaining));
        let end = bytes.len().min(written + 64 * 1024);
        match stream.write(&bytes[written..end]) {
            Ok(0) => return Err(WriteFailure::Io),
            Ok(count) => written += count,
            Err(error) if timed_out(&error) || error.kind() == ErrorKind::Interrupted => {}
            Err(_) => return Err(WriteFailure::Io),
        }
    }
    Ok(())
}

/// Ends the connection after a response: closes the write side, then discards what the peer
/// still sends (bounded in bytes and time), so an early refusal is read, not reset.
pub fn finish(stream: &mut TcpStream) {
    let _ = stream.shutdown(Shutdown::Write);
    let deadline = Instant::now() + DRAIN_DEADLINE;
    let mut chunk = [0u8; 8192];
    let mut drained = 0;
    while drained < DRAIN_BYTES {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let _ = stream.set_read_timeout(Some(remaining));
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(count) => drained += count,
        }
    }
    let _ = stream.shutdown(Shutdown::Both);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(text: &str) -> Result<Head, Failure> {
        parse_head(text.as_bytes(), 1000)
    }

    #[test]
    fn a_well_formed_head_is_parsed() {
        let head = head("POST /v1/execute HTTP/1.1\r\nHost: x\r\nContent-Length: 12").unwrap();
        assert_eq!(
            (
                head.method.as_str(),
                head.path.as_str(),
                head.content_length
            ),
            ("POST", "/v1/execute", 12)
        );
    }

    #[test]
    fn malformed_heads_are_refused() {
        for text in [
            "POST /v1/execute HTTP/1.1",
            "POST /v1/execute HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 1",
            "POST /v1/execute HTTP/1.1\r\nContent-Length: -1",
            "POST /v1/execute HTTP/1.1\r\nContent-Length: 1\r\nTransfer-Encoding: chunked",
            "post /v1/execute HTTP/1.1\r\nContent-Length: 1",
            "POST v1 HTTP/1.1\r\nContent-Length: 1",
            "POST /v1/execute HTTP/2\r\nContent-Length: 1",
            "POST /v1/execute HTTP/1.1\r\nbroken\r\nContent-Length: 1",
        ] {
            assert_eq!(head(text).err(), Some(Failure::Malformed), "{text}");
        }
    }

    #[test]
    fn a_body_above_the_listener_limit_is_refused_before_it_is_read() {
        assert_eq!(
            head("POST /v1/execute HTTP/1.1\r\nContent-Length: 1001").err(),
            Some(Failure::BodyTooLarge)
        );
    }
}
