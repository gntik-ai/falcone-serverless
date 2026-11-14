//! A deliberately small HTTP/1.1 layer: one request per connection, `Content-Length` bodies,
//! no keep-alive, no chunked encoding, no upgrades.
//!
//! Every phase has a bound, and none of them can be extended by the peer:
//!
//! - the request head is read under one absolute deadline and at most `MAX_HEADERS` bytes;
//! - the body is read at most `Content-Length` bytes (itself capped by the listener), and every
//!   read must make progress within the inactivity timeout;
//! - the response is written under one absolute deadline for the whole write, so a peer that
//!   reads one byte at a time is cut off at the deadline however large the response is.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::time::{Duration, Instant};

/// The largest request head (request line and headers).
pub const MAX_HEADERS: usize = 16 * 1024;
/// The largest request, which is the largest body the execute listener reads.
pub const MAX_REQUEST: usize = 8 * 1024 * 1024;
/// The largest body the control listener reads.
pub const MAX_CONTROL_BODY: usize = 64 * 1024;

/// The deadlines of one listener. Production values are fixed; a test build may use smaller ones.
#[derive(Clone, Copy)]
pub struct Timing {
    /// Absolute: from accept to the end of the request head.
    pub header_deadline: Duration,
    /// Inactivity: the longest a body read may wait for progress.
    pub body_inactivity: Duration,
    /// Absolute: from the first response byte to the last.
    pub response_deadline: Duration,
}

impl Timing {
    /// The execute listener's documented deadlines.
    pub const EXECUTE: Self = Self {
        header_deadline: Duration::from_secs(5),
        body_inactivity: Duration::from_secs(5),
        response_deadline: Duration::from_secs(5),
    };
    /// The control listener's documented deadlines.
    pub const CONTROL: Self = Self {
        header_deadline: Duration::from_secs(2),
        body_inactivity: Duration::from_secs(2),
        response_deadline: Duration::from_secs(2),
    };
}

/// The parsed request head.
pub struct Head {
    pub method: String,
    pub path: String,
    pub content_length: usize,
}

/// Why a request could not be read. None of them runs anything.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReadError {
    /// The peer closed, reset or stalled past a deadline: the connection is closed unanswered.
    Closed,
    /// The head is larger than `MAX_HEADERS`: answered `413`.
    HeadTooLarge,
    /// The head is not a request this layer serves: answered `400`.
    Malformed,
}

fn remaining(deadline: Instant) -> Option<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|left| !left.is_zero())
}

/// Reads the request head. Returns it with any body bytes that arrived in the same reads.
pub fn read_head(stream: &mut TcpStream, timing: &Timing) -> Result<(Head, Vec<u8>), ReadError> {
    let deadline = Instant::now() + timing.header_deadline;
    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];
    let end = loop {
        if let Some(at) = find_end(&buffer) {
            break at;
        }
        if buffer.len() > MAX_HEADERS {
            return Err(ReadError::HeadTooLarge);
        }
        let left = remaining(deadline).ok_or(ReadError::Closed)?;
        stream
            .set_read_timeout(Some(left))
            .map_err(|_| ReadError::Closed)?;
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return Err(ReadError::Closed),
            Ok(count) => buffer.extend_from_slice(&chunk[..count]),
        }
    };
    if end > MAX_HEADERS {
        return Err(ReadError::HeadTooLarge);
    }
    let head = parse_head(&buffer[..end])?;
    let rest = buffer[end + 4..].to_vec();
    Ok((head, rest))
}

fn find_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_head(bytes: &[u8]) -> Result<Head, ReadError> {
    let text = std::str::from_utf8(bytes).map_err(|_| ReadError::Malformed)?;
    let mut lines = text.split("\r\n");
    let mut request_line = lines.next().ok_or(ReadError::Malformed)?.split(' ');
    let (Some(method), Some(path), Some(version), None) = (
        request_line.next(),
        request_line.next(),
        request_line.next(),
        request_line.next(),
    ) else {
        return Err(ReadError::Malformed);
    };
    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return Err(ReadError::Malformed);
    }
    let mut content_length = None;
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(ReadError::Malformed)?;
        let (name, value) = (name.trim(), value.trim());
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(ReadError::Malformed);
        }
        if name.eq_ignore_ascii_case("content-length") {
            let parsed = value
                .bytes()
                .all(|b| b.is_ascii_digit())
                .then(|| value.parse::<usize>().ok())
                .flatten()
                .ok_or(ReadError::Malformed)?;
            if content_length.replace(parsed).is_some() {
                return Err(ReadError::Malformed);
            }
        }
    }
    Ok(Head {
        method: method.to_owned(),
        path: path.to_owned(),
        content_length: content_length.unwrap_or(0),
    })
}

/// Reads whatever already arrived without waiting, appending it to `body` up to `length`.
pub fn drain_available(stream: &mut TcpStream, body: &mut Vec<u8>, length: usize) {
    if stream.set_nonblocking(true).is_err() {
        return;
    }
    let mut chunk = [0u8; 16 * 1024];
    while body.len() < length {
        let want = chunk.len().min(length - body.len());
        match stream.read(&mut chunk[..want]) {
            Ok(0) | Err(_) => break,
            Ok(count) => body.extend_from_slice(&chunk[..count]),
        }
    }
    let _ = stream.set_nonblocking(false);
}

/// Completes a body of `length` bytes, `body` holding what already arrived. Every read waits at
/// most the inactivity timeout for progress; a peer that stalls is closed.
pub fn read_body(
    stream: &mut TcpStream,
    mut body: Vec<u8>,
    length: usize,
    timing: &Timing,
) -> Result<Vec<u8>, ReadError> {
    body.truncate(length);
    body.reserve(length.saturating_sub(body.len()).min(MAX_REQUEST));
    let mut chunk = vec![0u8; 64 * 1024];
    while body.len() < length {
        stream
            .set_read_timeout(Some(timing.body_inactivity))
            .map_err(|_| ReadError::Closed)?;
        let want = chunk.len().min(length - body.len());
        match stream.read(&mut chunk[..want]) {
            Ok(0) | Err(_) => return Err(ReadError::Closed),
            Ok(count) => body.extend_from_slice(&chunk[..count]),
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
        413 => "Content Too Large",
        422 => "Unprocessable Content",
        499 => "Client Closed Request",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Status",
    }
}

/// Writes one JSON response and closes the connection. The whole write, however many partial
/// writes it takes, ends at `timeout`; a peer that has not taken the response by then is cut
/// off and its connection is released.
pub fn respond(
    stream: &mut TcpStream,
    status: u16,
    body: &[u8],
    timeout: Duration,
) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    let head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        reason(status),
        body.len()
    );
    let result = write_until(stream, head.as_bytes(), deadline)
        .and_then(|()| write_until(stream, body, deadline));
    if result.is_ok() {
        close_gracefully(stream);
    } else {
        let _ = stream.shutdown(Shutdown::Both);
    }
    result
}

/// Ends the response with a FIN, then discards whatever the peer still sends for a moment.
/// Closing a socket that holds unread request bytes makes the kernel reset the connection, and
/// a reset can destroy a response the peer has not read yet; a refusal sent before the body was
/// read must reach the peer. The wait is short and bounded, and a peer that has closed ends it
/// at once.
fn close_gracefully(stream: &mut TcpStream) {
    let _ = stream.shutdown(Shutdown::Write);
    let end = Instant::now() + Duration::from_millis(200);
    let mut sink = [0u8; 8192];
    let mut drained = 0usize;
    while drained < 1024 * 1024 {
        let Some(left) = remaining(end) else { break };
        if stream.set_read_timeout(Some(left)).is_err() {
            break;
        }
        match stream.read(&mut sink) {
            Ok(0) | Err(_) => break,
            Ok(count) => drained += count,
        }
    }
}

/// As `respond`, for a listener's accept loop, which must never wait for a peer: a small
/// response, written without blocking, and whatever has already arrived is discarded.
pub fn respond_now(stream: &mut TcpStream, status: u16, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        reason(status),
        body.len()
    );
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(body);
    let _ = stream.set_write_timeout(Some(Duration::from_millis(100)));
    let _ = stream.write_all(&bytes);
    let _ = stream.shutdown(Shutdown::Write);
    discard_available(stream);
}

/// The most `respond_now` discards, and the longest it spends doing so: a peer that keeps its
/// receive queue full cannot hold an accept loop, which closes the socket when either bound is
/// reached.
const DISCARD_BYTES: usize = 64 * 1024;
const DISCARD_TIME: Duration = Duration::from_millis(20);

fn discard_available(stream: &mut TcpStream) {
    let _ = stream.set_nonblocking(true);
    discard_bounded(|sink| stream.read(sink));
}

/// Reads until the source is empty or closed, or a bound is reached; returns the bytes taken.
fn discard_bounded(mut read: impl FnMut(&mut [u8]) -> io::Result<usize>) -> usize {
    let end = Instant::now() + DISCARD_TIME;
    let mut sink = [0u8; 8192];
    let mut discarded = 0usize;
    while discarded < DISCARD_BYTES && remaining(end).is_some() {
        match read(&mut sink) {
            Ok(count) if count > 0 => discarded += count,
            _ => break,
        }
    }
    discarded
}

fn write_until(stream: &mut TcpStream, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        let left = remaining(deadline).ok_or(io::ErrorKind::TimedOut)?;
        stream.set_write_timeout(Some(left))?;
        match stream.write(bytes) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(count) => bytes = &bytes[count..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[test]
    fn discarding_stops_at_its_byte_bound_when_the_source_is_never_empty() {
        let taken = discard_bounded(|sink| Ok(sink.len()));
        assert!(
            (DISCARD_BYTES..DISCARD_BYTES + 8192).contains(&taken),
            "discarded {taken} bytes from an endless source"
        );
    }

    #[test]
    fn discarding_stops_at_its_time_bound_when_the_source_trickles() {
        let started = Instant::now();
        let taken = discard_bounded(|_| {
            std::thread::sleep(Duration::from_millis(5));
            Ok(1)
        });
        assert!(taken < DISCARD_BYTES);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    /// A peer that keeps streaming after the refusal must not hold the accept loop: the discard
    /// ends at its byte or time bound, and the socket is then closed.
    #[test]
    fn an_overflow_refusal_to_a_streaming_peer_returns_within_its_bound() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let sent = Arc::new(AtomicUsize::new(0));
        let stream = TcpStream::connect(address).unwrap();
        // Several writers keep the receive queue non-empty while the refusal is discarding.
        let peers: Vec<_> = (0..4)
            .map(|_| {
                let (stop, sent) = (stop.clone(), sent.clone());
                let mut stream = stream.try_clone().unwrap();
                std::thread::spawn(move || {
                    let chunk = [b'x'; 8192];
                    while !stop.load(Ordering::Acquire) {
                        match stream.write(&chunk) {
                            Ok(count) => {
                                sent.fetch_add(count, Ordering::AcqRel);
                            }
                            Err(_) => break,
                        }
                    }
                })
            })
            .collect();
        let (mut accepted, _) = listener.accept().unwrap();
        // The peer is demonstrably streaming before the refusal is written.
        while sent.load(Ordering::Acquire) < 4 * DISCARD_BYTES {
            std::thread::yield_now();
        }
        let started = Instant::now();
        respond_now(&mut accepted, 503, b"{}");
        let took = started.elapsed();
        drop(accepted);
        stop.store(true, Ordering::Release);
        drop(stream);
        for peer in peers {
            peer.join().unwrap();
        }
        assert!(
            took < Duration::from_secs(1),
            "the overflow refusal held the accept loop for {took:?}"
        );
    }
}
