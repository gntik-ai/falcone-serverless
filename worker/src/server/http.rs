//! A deliberately small HTTP/1.1 layer: one request per connection, every read and write
//! under an absolute deadline.
//!
//! A deadline here is an instant, not a per-call timeout: each read or write is given only
//! the time that is left, so a peer that sends or reads one byte at a time cannot extend it.
//! The layer has no keep-alive, no chunked bodies and no upgrade.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::time::{Duration, Instant};

/// The size limits and absolute deadlines of one listener.
#[derive(Clone, Copy, Debug)]
pub struct ListenerLimits {
    /// Connections handled at once; one more is answered `503` and closed.
    pub max_connections: usize,
    /// The request line and headers together.
    pub max_header_bytes: usize,
    /// The request body.
    pub max_body_bytes: usize,
    /// From accept to the end of the headers.
    pub header_deadline: Duration,
    /// From the end of the headers to the end of the body.
    pub body_deadline: Duration,
    /// From the first byte of the response to its last.
    pub response_deadline: Duration,
}

/// Connections on the execute listener beyond `concurrency`: requests between accept and
/// admission, which hold no execution slot.
pub const ADMISSION_CONNECTIONS: usize = 8;

impl ListenerLimits {
    /// The execute listener: `concurrency` running executions plus a fixed number of
    /// connections in admission.
    pub const fn execute(concurrency: usize) -> Self {
        Self {
            max_connections: concurrency + ADMISSION_CONNECTIONS,
            max_header_bytes: 16 * 1024,
            max_body_bytes: 8 * 1024 * 1024,
            header_deadline: Duration::from_secs(5),
            body_deadline: Duration::from_secs(10),
            response_deadline: Duration::from_secs(5),
        }
    }

    /// The control listener: cancel and update only, small messages, tight deadlines, and a
    /// connection limit of its own that no execute traffic can touch.
    pub const fn control() -> Self {
        Self {
            max_connections: 16,
            max_header_bytes: 4 * 1024,
            max_body_bytes: 64 * 1024,
            header_deadline: Duration::from_secs(2),
            body_deadline: Duration::from_secs(2),
            response_deadline: Duration::from_secs(2),
        }
    }
}

/// Why a request was not read.
#[derive(Debug, PartialEq, Eq)]
pub enum Fault {
    /// End of stream, an I/O error, or a deadline: the connection is just closed.
    Closed,
    /// The request line and headers exceed the limit.
    HeadersTooLarge,
    /// Not an HTTP/1.1 request this layer serves.
    Malformed,
}

/// The request line and the headers this layer uses.
pub struct Head {
    pub method: String,
    pub path: String,
    pub content_length: Option<usize>,
    pub expects_continue: bool,
    /// Body bytes that arrived with the headers.
    pub buffered: Vec<u8>,
}

fn remaining(deadline: Instant) -> Option<Duration> {
    let left = deadline.saturating_duration_since(Instant::now());
    (!left.is_zero()).then_some(left.max(Duration::from_millis(1)))
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|at| at + from)
}

/// Reads the request line and headers, giving up at `deadline`.
pub fn read_head(
    stream: &mut TcpStream,
    max_header_bytes: usize,
    deadline: Instant,
) -> Result<Head, Fault> {
    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0u8; 2048];
    let mut scanned = 0;
    let end = loop {
        if let Some(at) = find(&buffer, b"\r\n\r\n", scanned) {
            break at;
        }
        scanned = buffer.len().saturating_sub(3);
        if buffer.len() > max_header_bytes {
            return Err(Fault::HeadersTooLarge);
        }
        let left = remaining(deadline).ok_or(Fault::Closed)?;
        stream
            .set_read_timeout(Some(left))
            .map_err(|_| Fault::Closed)?;
        match stream.read(&mut chunk) {
            Ok(0) => return Err(Fault::Closed),
            Ok(count) => buffer.extend_from_slice(&chunk[..count]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(Fault::Closed),
        }
    };
    if end + 4 > max_header_bytes {
        return Err(Fault::HeadersTooLarge);
    }
    let text = std::str::from_utf8(&buffer[..end]).map_err(|_| Fault::Malformed)?;
    let mut lines = text.split("\r\n");
    let mut request_line = lines.next().ok_or(Fault::Malformed)?.split(' ');
    let (Some(method), Some(path), Some(version), None) = (
        request_line.next(),
        request_line.next(),
        request_line.next(),
        request_line.next(),
    ) else {
        return Err(Fault::Malformed);
    };
    if !matches!(version, "HTTP/1.1" | "HTTP/1.0") || method.is_empty() || path.is_empty() {
        return Err(Fault::Malformed);
    }
    let mut content_length = None;
    let mut expects_continue = false;
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(Fault::Malformed)?;
        let value = value.trim();
        match name.to_ascii_lowercase().as_str() {
            "content-length" => {
                if content_length.is_some()
                    || value.is_empty()
                    || !value.bytes().all(|b| b.is_ascii_digit())
                {
                    return Err(Fault::Malformed);
                }
                content_length = Some(value.parse::<usize>().map_err(|_| Fault::Malformed)?);
            }
            // Chunked and other transfer codings are not served.
            "transfer-encoding" => return Err(Fault::Malformed),
            "expect" => expects_continue = value.eq_ignore_ascii_case("100-continue"),
            _ => {}
        }
    }
    Ok(Head {
        method: method.to_owned(),
        path: path.to_owned(),
        content_length,
        expects_continue,
        buffered: buffer[end + 4..].to_vec(),
    })
}

/// Reads exactly `length` body bytes, giving up at `deadline`.
pub fn read_body(
    stream: &mut TcpStream,
    head: &mut Head,
    length: usize,
    deadline: Instant,
) -> Result<Vec<u8>, Fault> {
    let mut body = std::mem::take(&mut head.buffered);
    body.truncate(length);
    body.reserve_exact(length.saturating_sub(body.len()).min(1 << 20));
    let mut chunk = vec![0u8; 64 * 1024];
    while body.len() < length {
        let left = remaining(deadline).ok_or(Fault::Closed)?;
        stream
            .set_read_timeout(Some(left))
            .map_err(|_| Fault::Closed)?;
        let want = (length - body.len()).min(chunk.len());
        match stream.read(&mut chunk[..want]) {
            Ok(0) => return Err(Fault::Closed),
            Ok(count) => body.extend_from_slice(&chunk[..count]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(Fault::Closed),
        }
    }
    Ok(body)
}

/// Collects body bytes that are on their way, for at most `grace`, without waiting for a
/// body that does not come: it stops at the first read that gets nothing within what is left
/// of `grace`. Returns whether `length` bytes are now in `head.buffered`. The head reader
/// stops at the end of the headers, so a request that was sent whole may still be partly in
/// the socket, or still arriving in fragments, when it returns.
pub fn collect_arriving(
    stream: &mut TcpStream,
    head: &mut Head,
    length: usize,
    grace: Duration,
) -> bool {
    let deadline = Instant::now() + grace;
    let mut chunk = [0u8; 16 * 1024];
    while head.buffered.len() < length {
        let Some(left) = remaining(deadline) else {
            break;
        };
        if stream.set_read_timeout(Some(left)).is_err() {
            break;
        }
        let want = (length - head.buffered.len()).min(chunk.len());
        match stream.read(&mut chunk[..want]) {
            Ok(0) | Err(_) => break,
            Ok(count) => head.buffered.extend_from_slice(&chunk[..count]),
        }
    }
    head.buffered.len() >= length
}

fn write_until(stream: &mut TcpStream, bytes: &[u8], deadline: Instant) -> bool {
    let mut written = 0;
    while written < bytes.len() {
        let Some(left) = remaining(deadline) else {
            return false;
        };
        if stream.set_write_timeout(Some(left)).is_err() {
            return false;
        }
        match stream.write(&bytes[written..]) {
            Ok(0) => return false,
            Ok(count) => written += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
    true
}

fn reason(status: u16) -> &'static str {
    match status {
        100 => "Continue",
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        408 => "Request Timeout",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Content Too Large",
        422 => "Unprocessable Content",
        431 => "Request Header Fields Too Large",
        499 => "Client Closed Request",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Status",
    }
}

/// Writes the whole response within `response_deadline` of now, or gives up and returns
/// false. One absolute deadline covers every partial write, so a reader that takes the
/// response a byte at a time is cut off when the deadline passes.
pub fn respond(
    stream: &mut TcpStream,
    status: u16,
    body: &[u8],
    response_deadline: Duration,
) -> bool {
    let deadline = Instant::now() + response_deadline;
    let head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        reason(status),
        body.len()
    );
    write_until(stream, head.as_bytes(), deadline) && write_until(stream, body, deadline)
}

/// `100 Continue`, for a client that waits for it before sending the body.
pub fn respond_continue(stream: &mut TcpStream, response_deadline: Duration) -> bool {
    write_until(
        stream,
        b"HTTP/1.1 100 Continue\r\n\r\n",
        Instant::now() + response_deadline,
    )
}

/// Ends the connection after a response. A client that is still sending is read for a
/// moment (bounded) so that closing does not reset the connection and discard the response
/// it has not read yet.
pub fn finish(stream: &mut TcpStream) {
    let _ = stream.shutdown(Shutdown::Write);
    let deadline = Instant::now() + Duration::from_millis(100);
    let mut sink = [0u8; 4096];
    let mut drained = 0usize;
    while drained < 1024 * 1024 {
        let Some(left) = remaining(deadline) else {
            break;
        };
        if stream.set_read_timeout(Some(left)).is_err() {
            break;
        }
        match stream.read(&mut sink) {
            Ok(0) | Err(_) => break,
            Ok(count) => drained += count,
        }
    }
    let _ = stream.shutdown(Shutdown::Both);
}
