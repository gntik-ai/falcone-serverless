//! A minimal HTTP/1.1 reader and writer over `std::net`: one request per connection, bounded
//! head and body, no chunked bodies and no keep-alive.

use std::io::{Read, Write};
use std::time::Instant;

/// Request line and headers.
const MAX_HEAD_BYTES: usize = 16 * 1024;
/// The whole execute message: credential, projection and `params` with a body of up to 1 MiB.
pub const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

pub struct Request {
    pub method: String,
    pub path: String,
    pub body: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReadError {
    /// Not an HTTP/1.x request this server accepts.
    Malformed,
    /// A declared body above [`MAX_BODY_BYTES`]; nothing of it was read.
    TooLarge,
    /// The peer closed or timed out before a full request arrived.
    Closed,
}

fn head_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

/// Reads one request, giving up when `deadline` passes: a peer that drips bytes cannot hold
/// a connection longer than this, whatever the per-read timeout.
pub fn read_request(stream: &mut impl Read, deadline: Instant) -> Result<Request, ReadError> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let end = loop {
        if let Some(end) = head_end(&buffer) {
            break end;
        }
        if buffer.len() > MAX_HEAD_BYTES {
            return Err(ReadError::Malformed);
        }
        if Instant::now() >= deadline {
            return Err(ReadError::Closed);
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return Err(ReadError::Closed),
            Ok(count) => buffer.extend_from_slice(&chunk[..count]),
        }
    };
    if end > MAX_HEAD_BYTES {
        return Err(ReadError::Malformed);
    }
    let head = std::str::from_utf8(&buffer[..end]).map_err(|_| ReadError::Malformed)?;
    let mut lines = head.split("\r\n");
    let mut parts = lines.next().unwrap_or_default().split(' ');
    let (method, path, version) = (parts.next(), parts.next(), parts.next());
    let (Some(method), Some(path), Some(version), None) = (method, path, version, parts.next())
    else {
        return Err(ReadError::Malformed);
    };
    if !matches!(version, "HTTP/1.1" | "HTTP/1.0") || method.is_empty() || !path.starts_with('/') {
        return Err(ReadError::Malformed);
    }
    let mut length = None;
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(ReadError::Malformed)?;
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(ReadError::Malformed);
        }
        if name.eq_ignore_ascii_case("content-length") {
            let value = value.trim();
            if length.is_some()
                || value.is_empty()
                || value.len() > 12
                || !value.bytes().all(|b| b.is_ascii_digit())
            {
                return Err(ReadError::Malformed);
            }
            length = value.parse::<usize>().ok();
            if length.is_none() {
                return Err(ReadError::Malformed);
            }
        }
    }
    let length = length.unwrap_or(0);
    if length > MAX_BODY_BYTES {
        return Err(ReadError::TooLarge);
    }
    let (method, path) = (method.to_owned(), path.to_owned());
    let mut body = buffer.split_off(end + 4);
    if body.len() > length {
        return Err(ReadError::Malformed);
    }
    while body.len() < length {
        if Instant::now() >= deadline {
            return Err(ReadError::Closed);
        }
        let want = (length - body.len()).min(chunk.len());
        match stream.read(&mut chunk[..want]) {
            Ok(0) | Err(_) => return Err(ReadError::Closed),
            Ok(count) => body.extend_from_slice(&chunk[..count]),
        }
    }
    Ok(Request { method, path, body })
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Content Too Large",
        422 => "Unprocessable Content",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Error",
    }
}

pub fn write_response(stream: &mut impl Write, status: u16, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n",
        reason(status),
        body.len()
    );
    let _ = stream
        .write_all(head.as_bytes())
        .and_then(|()| stream.write_all(body))
        .and_then(|()| stream.flush());
}

/// Reads and discards what the peer still sends, up to `limit` bytes or `deadline`, so that
/// closing the connection after an early answer does not reset it and lose that answer.
pub fn drain(stream: &mut impl Read, limit: usize, deadline: Instant) {
    let mut chunk = [0u8; 8192];
    let mut total = 0;
    while total < limit && Instant::now() < deadline {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(count) => total += count,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(input: &[u8]) -> Result<Request, ReadError> {
        read_request(
            &mut &input[..],
            Instant::now() + std::time::Duration::from_secs(60),
        )
    }

    #[test]
    fn reads_a_post_with_its_body() {
        let request = read(b"POST /v1/cancel HTTP/1.1\r\nContent-Length: 2\r\n\r\n{}").unwrap();
        assert_eq!(
            (request.method.as_str(), request.path.as_str(), request.body),
            ("POST", "/v1/cancel", b"{}".to_vec())
        );
    }

    #[test]
    fn refuses_what_it_does_not_serve() {
        for input in [
            &b"POST /v1/execute HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n"[..],
            b"POST /v1/execute HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\nx",
            b"POST /v1/execute HTTP/1.1\r\nContent-Length: -1\r\n\r\n",
            b"POST /v1/execute HTTP/2\r\n\r\n",
            b"POST v1 HTTP/1.1\r\n\r\n",
            b"POST /v1/execute HTTP/1.1\r\nContent-Length: 1\r\n\r\nxx",
        ] {
            assert_eq!(read(input).err(), Some(ReadError::Malformed));
        }
    }

    #[test]
    fn a_body_above_the_bound_is_refused_unread() {
        let head = format!(
            "POST /v1/execute HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY_BYTES + 1
        );
        assert_eq!(read(head.as_bytes()).err(), Some(ReadError::TooLarge));
    }

    #[test]
    fn a_request_that_outlives_its_deadline_is_closed() {
        let past = Instant::now() - std::time::Duration::from_secs(1);
        let input = b"POST /v1/execute HTTP/1.1\r\nContent-Length: 2\r\n\r\n{}";
        assert_eq!(
            read_request(&mut &input[..], past).err(),
            Some(ReadError::Closed)
        );
    }

    #[test]
    fn a_truncated_request_is_closed() {
        assert_eq!(
            read(b"POST /v1/execute HTTP/1.1\r\nContent-Length: 5\r\n\r\nab").err(),
            Some(ReadError::Closed)
        );
    }
}
