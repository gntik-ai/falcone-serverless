//! A minimal HTTP/1.1 reader and writer for the two listeners: one request per connection,
//! hard size ceilings, and an absolute deadline for every phase. Nothing here parses JSON or
//! reads a credential; a request value never reaches a formatted error.

use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

/// The header block, request line and terminating blank line included.
pub const MAX_HEADERS: usize = 16 * 1024;
/// The largest request body the execute listener reads (8 MiB).
pub const MAX_REQUEST: usize = 8 * 1024 * 1024;
/// The largest request body the control listener reads (cancel and update are tiny).
pub const MAX_CONTROL_BODY: usize = 64 * 1024;
/// A client that does not read its response is closed after this long.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// After a response, unread request bytes are discarded for at most this long and this much, so
/// closing does not reset the connection before the client has read the response.
const DRAIN_TIME: Duration = Duration::from_millis(250);
const DRAIN_BYTES: usize = 256 * 1024;

/// A connection the reader can bound in time.
pub trait Stream: Read + Write {
    /// The longest a single `read` may block; `Duration::ZERO` is never passed.
    fn read_timeout(&mut self, limit: Duration) -> io::Result<()>;
    fn write_timeout(&mut self, limit: Duration) -> io::Result<()>;
    fn shutdown_write(&mut self) -> io::Result<()>;
}

impl Stream for std::net::TcpStream {
    fn read_timeout(&mut self, limit: Duration) -> io::Result<()> {
        self.set_read_timeout(Some(limit))
    }

    fn write_timeout(&mut self, limit: Duration) -> io::Result<()> {
        self.set_write_timeout(Some(limit))
    }

    fn shutdown_write(&mut self) -> io::Result<()> {
        self.shutdown(std::net::Shutdown::Write)
    }
}

/// Why a request was not read in full. None of them carries request data.
#[derive(Debug, PartialEq, Eq)]
pub enum Fault {
    /// The peer closed or reset before a complete request; nothing to answer.
    Closed,
    /// A deadline passed; the connection is closed without an answer.
    TimedOut,
    HeadersTooLarge,
    /// Not a request this server understands; the label is fixed text.
    Malformed(&'static str),
}

#[derive(Debug, PartialEq, Eq)]
pub struct Head {
    pub method: String,
    pub path: String,
    pub content_length: Option<usize>,
}

/// Reads the request head until `deadline`, an absolute instant from accepting the connection.
/// Returns the head and the body bytes that arrived in the same reads.
pub fn read_head(stream: &mut impl Stream, deadline: Instant) -> Result<(Head, Vec<u8>), Fault> {
    let mut buffer: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 4096];
    let mut scanned = 0_usize;
    loop {
        // A terminator can straddle two reads, so rescan the last three scanned bytes.
        let from = scanned.saturating_sub(3);
        scanned = buffer.len();
        if let Some(at) = buffer[from..]
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
        {
            let end = from + at + 4;
            if end > MAX_HEADERS {
                return Err(Fault::HeadersTooLarge);
            }
            let head = parse_head(&buffer[..end - 4])?;
            return Ok((head, buffer.split_off(end)));
        }
        if buffer.len() >= MAX_HEADERS {
            return Err(Fault::HeadersTooLarge);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Fault::TimedOut);
        }
        stream.read_timeout(remaining).map_err(|_| Fault::Closed)?;
        match stream.read(&mut chunk) {
            Ok(0) => return Err(Fault::Closed),
            Ok(count) => buffer.extend_from_slice(&chunk[..count]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if timed_out(&error) => return Err(Fault::TimedOut),
            Err(_) => return Err(Fault::Closed),
        }
    }
}

fn timed_out(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

fn parse_head(block: &[u8]) -> Result<Head, Fault> {
    let malformed = Fault::Malformed;
    let text = std::str::from_utf8(block).map_err(|_| malformed("head_not_utf8"))?;
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split(' ');
    let (Some(method), Some(path), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(malformed("request_line"));
    };
    if method.is_empty() || !method.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(malformed("method"));
    }
    if !path.starts_with('/') || path.bytes().any(|b| b.is_ascii_control() || b == b' ') {
        return Err(malformed("target"));
    }
    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return Err(malformed("version"));
    }
    let mut content_length = None;
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(malformed("header"))?;
        if name.is_empty()
            || name
                .bytes()
                .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
        {
            return Err(malformed("header"));
        }
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(malformed("transfer_encoding"));
        }
        if name.eq_ignore_ascii_case("content-length") {
            let value = value.trim();
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(malformed("content_length"));
            }
            // A value too large for usize is larger than every ceiling.
            let parsed = value.parse::<usize>().unwrap_or(usize::MAX);
            if content_length.is_some_and(|earlier| earlier != parsed) {
                return Err(malformed("content_length"));
            }
            content_length = Some(parsed);
        }
    }
    Ok(Head {
        method: method.to_owned(),
        path: path.to_owned(),
        content_length,
    })
}

/// Reads exactly `length` body bytes. `idle` bounds each read and `total` the whole body, both
/// from now; `already` holds the bytes that arrived with the head.
pub fn read_body(
    stream: &mut impl Stream,
    length: usize,
    mut already: Vec<u8>,
    idle: Duration,
    total: Duration,
) -> Result<Vec<u8>, Fault> {
    already.truncate(length);
    let deadline = Instant::now() + total;
    let mut chunk = vec![0_u8; 64 * 1024];
    while already.len() < length {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Fault::TimedOut);
        }
        stream
            .read_timeout(remaining.min(idle))
            .map_err(|_| Fault::Closed)?;
        let want = chunk.len().min(length - already.len());
        match stream.read(&mut chunk[..want]) {
            Ok(0) => return Err(Fault::Closed),
            Ok(count) => already.extend_from_slice(&chunk[..count]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if timed_out(&error) => return Err(Fault::TimedOut),
            Err(_) => return Err(Fault::Closed),
        }
    }
    Ok(already)
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Content Too Large",
        422 => "Unprocessable Content",
        424 => "Failed Dependency",
        431 => "Request Header Fields Too Large",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Internal Server Error",
    }
}

/// Writes a JSON response and asks the peer to close. It never waits longer than
/// `WRITE_TIMEOUT` for a client that does not read.
pub fn respond(stream: &mut impl Stream, status: u16, body: &[u8]) -> io::Result<()> {
    stream.write_timeout(WRITE_TIMEOUT)?;
    let head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        reason(status),
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

/// Ends the connection after a response: half-close, then discard what the client still sends
/// for a bounded time so the close is not a reset that could swallow the response.
pub fn finish(stream: &mut impl Stream) {
    let _ = stream.shutdown_write();
    let deadline = Instant::now() + DRAIN_TIME;
    let mut discarded = 0;
    let mut chunk = [0_u8; 4096];
    while discarded < DRAIN_BYTES {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || stream.read_timeout(remaining).is_err() {
            return;
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(count) => discarded += count,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// An in-memory peer: reads come from `input`, writes are collected.
    struct Peer {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
    }

    impl Peer {
        fn new(input: &[u8]) -> Self {
            Self {
                input: Cursor::new(input.to_vec()),
                output: Vec::new(),
            }
        }
    }

    impl Read for Peer {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.input.read(buffer)
        }
    }

    impl Write for Peer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Stream for Peer {
        fn read_timeout(&mut self, _: Duration) -> io::Result<()> {
            Ok(())
        }

        fn write_timeout(&mut self, _: Duration) -> io::Result<()> {
            Ok(())
        }

        fn shutdown_write(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn later() -> Instant {
        Instant::now() + Duration::from_secs(60)
    }

    #[test]
    fn a_post_with_a_body_splits_into_head_and_early_body_bytes() {
        let mut peer =
            Peer::new(b"POST /v1/execute HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\n\r\nab");
        let (head, early) = read_head(&mut peer, later()).unwrap();
        assert_eq!(
            head,
            Head {
                method: "POST".into(),
                path: "/v1/execute".into(),
                content_length: Some(5)
            }
        );
        assert_eq!(early, b"ab");
    }

    #[test]
    fn the_header_ceiling_is_exactly_sixteen_kib() {
        let padded = |total: usize| {
            let line = "POST /v1/execute HTTP/1.1\r\nX-Pad: ";
            let fill = total - line.len() - 4;
            let mut bytes = line.as_bytes().to_vec();
            bytes.extend(std::iter::repeat_n(b'a', fill));
            bytes.extend_from_slice(b"\r\n\r\n");
            bytes
        };
        assert!(read_head(&mut Peer::new(&padded(MAX_HEADERS)), later()).is_ok());
        assert_eq!(
            read_head(&mut Peer::new(&padded(MAX_HEADERS + 1)), later()).unwrap_err(),
            Fault::HeadersTooLarge
        );
        // No terminator at all: the ceiling still applies.
        let endless = vec![b'a'; MAX_HEADERS + 100];
        assert_eq!(
            read_head(&mut Peer::new(&endless), later()).unwrap_err(),
            Fault::HeadersTooLarge
        );
    }

    #[test]
    fn a_closed_or_truncated_head_is_closed_not_answered() {
        for input in [
            &b""[..],
            b"POST /v1/exec",
            b"POST /v1/execute HTTP/1.1\r\nHost: x\r\n",
        ] {
            assert_eq!(
                read_head(&mut Peer::new(input), later()).unwrap_err(),
                Fault::Closed
            );
        }
    }

    #[test]
    fn an_expired_deadline_times_out_before_reading() {
        let mut peer = Peer::new(b"POST / HTTP/1.1\r\n");
        let expired = Instant::now() - Duration::from_secs(1);
        assert_eq!(read_head(&mut peer, expired).unwrap_err(), Fault::TimedOut);
    }

    #[test]
    fn malformed_heads_are_refused_with_a_fixed_label() {
        for (input, label) in [
            (&b"post / HTTP/1.1\r\n\r\n"[..], "method"),
            (b"POST / HTTP/2\r\n\r\n", "version"),
            (b"POST nothing HTTP/1.1\r\n\r\n", "target"),
            (b"POST / HTTP/1.1 extra\r\n\r\n", "request_line"),
            (b"POST / HTTP/1.1\r\nNoColon\r\n\r\n", "header"),
            (
                b"POST / HTTP/1.1\r\nContent-Length: -1\r\n\r\n",
                "content_length",
            ),
            (
                b"POST / HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\n",
                "content_length",
            ),
            (
                b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n",
                "transfer_encoding",
            ),
        ] {
            assert_eq!(
                read_head(&mut Peer::new(input), later()).unwrap_err(),
                Fault::Malformed(label),
                "{}",
                String::from_utf8_lossy(input)
            );
        }
    }

    #[test]
    fn a_huge_content_length_parses_as_larger_than_every_ceiling() {
        let input = b"POST / HTTP/1.1\r\nContent-Length: 99999999999999999999999\r\n\r\n";
        let (head, _) = read_head(&mut Peer::new(input), later()).unwrap();
        assert_eq!(head.content_length, Some(usize::MAX));
    }

    #[test]
    fn a_body_is_read_to_its_length_and_a_short_body_is_closed() {
        let mut peer = Peer::new(b"cdef");
        let body = read_body(
            &mut peer,
            6,
            b"ab".to_vec(),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(body, b"abcdef");
        let mut short = Peer::new(b"cd");
        assert_eq!(
            read_body(
                &mut short,
                6,
                b"ab".to_vec(),
                Duration::from_secs(1),
                Duration::from_secs(1)
            )
            .unwrap_err(),
            Fault::Closed
        );
    }

    #[test]
    fn a_response_carries_status_length_and_close() {
        let mut peer = Peer::new(b"");
        respond(&mut peer, 503, b"{\"error\":\"busy\"}").unwrap();
        let text = String::from_utf8(peer.output).unwrap();
        assert!(
            text.starts_with("HTTP/1.1 503 Service Unavailable\r\n"),
            "{text}"
        );
        assert!(text.contains("Content-Length: 16\r\n"), "{text}");
        assert!(text.contains("Connection: close\r\n"), "{text}");
        assert!(text.ends_with("\r\n\r\n{\"error\":\"busy\"}"), "{text}");
    }
}
