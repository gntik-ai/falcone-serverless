//! The HTTP/1.1 subset of the pod-local worker API: one POST per connection, the connection is
//! always closed after the reply. Every size ceiling and every transfer deadline lives here.
//!
//! - Headers: the request line and header lines are at most `MAX_HEADERS` bytes, and the whole
//!   section must arrive within the absolute `HEADER_DEADLINE`, however slowly it is dripped.
//! - Request: head plus body is at most `MAX_REQUEST` bytes for execute; control messages (cancel,
//!   update) have their own, much smaller body ceiling. A body must be announced by
//!   `Content-Length`; chunked bodies are not supported.
//! - Inactivity: a read that makes no progress for `IO_TIMEOUT` closes the connection, and the
//!   body as a whole must arrive within `BODY_DEADLINE`.
//!
//! A refusal is a status line without a body, except 413, whose body is the contract's
//! `request_too_large` envelope. Nothing here reads a header value other than `Content-Length`
//! and `Transfer-Encoding`, and no error carries a request byte.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::time::{Duration, Instant};

/// The request line and headers, up to and including the blank line.
pub const MAX_HEADERS: usize = 16 * 1024;
/// Head plus body of an execute request.
pub const MAX_REQUEST: usize = 8 * 1024 * 1024;
/// The body of a cancel or update message.
pub const MAX_CONTROL_BODY: usize = 16 * 1024;
/// The absolute time the header section may take.
pub const HEADER_DEADLINE: Duration = Duration::from_secs(5);
/// The longest a read or a write may make no progress.
pub const IO_TIMEOUT: Duration = Duration::from_secs(5);
/// The absolute time the body, and separately the reply, may take.
pub const BODY_DEADLINE: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug)]
pub struct HttpLimits {
    pub max_headers: usize,
    pub max_request: usize,
    pub max_control_body: usize,
    pub header_deadline: Duration,
    pub io_timeout: Duration,
    pub body_deadline: Duration,
}

impl Default for HttpLimits {
    fn default() -> Self {
        Self {
            max_headers: MAX_HEADERS,
            max_request: MAX_REQUEST,
            max_control_body: MAX_CONTROL_BODY,
            header_deadline: HEADER_DEADLINE,
            io_timeout: IO_TIMEOUT,
            body_deadline: BODY_DEADLINE,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Route {
    Execute,
    Update,
    Cancel,
}

/// Why a request is not served.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reject {
    /// Close the connection without a reply (silence, a deadline, a broken socket).
    Close,
    /// Reply with this status and no body.
    Status(u16),
    /// Reply 413 with the contract's `request_too_large` envelope.
    TooLarge,
}

/// A parsed request head: the route, the announced body length, and any body bytes that arrived
/// with the head.
pub struct Head {
    pub route: Route,
    pub length: usize,
    leftover: Vec<u8>,
}

fn remaining(deadline: Instant, io_timeout: Duration) -> Result<Duration, Reject> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|left| !left.is_zero())
        .map(|left| left.min(io_timeout))
        .ok_or(Reject::Close)
}

fn terminator(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

/// Reads and checks the head. The body is not read, so a request refused here costs only the
/// head.
pub fn read_head(stream: &mut TcpStream, limits: &HttpLimits) -> Result<Head, Reject> {
    let deadline = Instant::now() + limits.header_deadline;
    let mut buffer: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 2048];
    let end = loop {
        if let Some(at) = terminator(&buffer) {
            if at + 4 > limits.max_headers {
                return Err(Reject::Status(431));
            }
            break at + 4;
        }
        if buffer.len() >= limits.max_headers {
            return Err(Reject::Status(431));
        }
        let wait = remaining(deadline, limits.io_timeout)?;
        stream
            .set_read_timeout(Some(wait))
            .map_err(|_| Reject::Close)?;
        let read = match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return Err(Reject::Close),
            Ok(read) => read,
        };
        buffer.extend_from_slice(&chunk[..read]);
    };
    let text = std::str::from_utf8(&buffer[..end - 4]).map_err(|_| Reject::Status(400))?;
    let mut lines = text.split("\r\n");
    let mut parts = lines.next().unwrap_or_default().split(' ');
    let (Some(method), Some(target), Some("HTTP/1.1"), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(Reject::Status(400));
    };
    let route = match target {
        "/v1/execute" => Route::Execute,
        "/v1/update" => Route::Update,
        "/v1/cancel" => Route::Cancel,
        _ => return Err(Reject::Status(404)),
    };
    if method != "POST" {
        return Err(Reject::Status(405));
    }
    let mut length: Option<usize> = None;
    let mut chunked = false;
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(Reject::Status(400))?;
        if name.eq_ignore_ascii_case("content-length") {
            let value = value.trim();
            if length.is_some()
                || value.is_empty()
                || value.len() > 12
                || !value.bytes().all(|b| b.is_ascii_digit())
            {
                return Err(Reject::Status(400));
            }
            length = value.parse().ok();
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            chunked = true;
        }
    }
    if chunked {
        return Err(Reject::Status(501));
    }
    let length = length.ok_or(Reject::Status(411))?;
    let too_large = match route {
        Route::Execute => length > limits.max_request.saturating_sub(end),
        Route::Update | Route::Cancel => length > limits.max_control_body,
    };
    if too_large {
        return Err(Reject::TooLarge);
    }
    let mut leftover = buffer.split_off(end);
    leftover.truncate(length);
    Ok(Head {
        route,
        length,
        leftover,
    })
}

/// Reads the announced body within `BODY_DEADLINE`, closing when a read stalls.
pub fn read_body(
    stream: &mut TcpStream,
    head: Head,
    limits: &HttpLimits,
) -> Result<Vec<u8>, Reject> {
    let deadline = Instant::now() + limits.body_deadline;
    let mut body = head.leftover;
    let mut chunk = [0u8; 16 * 1024];
    while body.len() < head.length {
        let wait = remaining(deadline, limits.io_timeout)?;
        stream
            .set_read_timeout(Some(wait))
            .map_err(|_| Reject::Close)?;
        let want = chunk.len().min(head.length - body.len());
        match stream.read(&mut chunk[..want]) {
            Ok(0) | Err(_) => return Err(Reject::Close),
            Ok(read) => body.extend_from_slice(&chunk[..read]),
        }
    }
    Ok(body)
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Content Too Large",
        422 => "Unprocessable Content",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

/// Writes the reply within the write deadline, then closes the connection. A peer that does not
/// read it is cut off after `IO_TIMEOUT` of no progress or `BODY_DEADLINE` in total.
pub fn respond(
    stream: &mut TcpStream,
    status: u16,
    body: &[u8],
    limits: &HttpLimits,
) -> io::Result<()> {
    let retry = if status == 503 {
        "Retry-After: 1\r\n"
    } else {
        ""
    };
    let mut bytes = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{retry}Connection: close\r\n\r\n",
        reason(status),
        body.len()
    )
    .into_bytes();
    bytes.extend_from_slice(body);
    let deadline = Instant::now() + limits.body_deadline;
    let mut written = 0;
    while written < bytes.len() {
        let wait = remaining(deadline, limits.io_timeout)
            .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))?;
        stream.set_write_timeout(Some(wait))?;
        match stream.write(&bytes[written..]) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero)),
            Ok(count) => written += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    stream.flush()
}

/// After a reply sent before the body was read: stop sending, then briefly discard what is in
/// flight, so the peer sees the reply and not a reset. Bounded in time and in bytes.
pub fn close(stream: &mut TcpStream) {
    let _ = stream.shutdown(Shutdown::Write);
    let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
    let mut scratch = [0u8; 4096];
    let mut left: usize = 16 * 1024;
    while left > 0 {
        match stream.read(&mut scratch) {
            Ok(0) | Err(_) => break,
            Ok(read) => left = left.saturating_sub(read),
        }
    }
}
