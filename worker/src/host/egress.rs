//! The HTTP egress gate: allow-list, one resolution per hop, address classification, and a
//! plain-HTTP/1.1 exchange over the validated socket address.
//!
//! The client connects to the `SocketAddr` that was validated and never hands a name to a
//! connector, so a second DNS answer cannot redirect the connection. Every redirect hop is
//! parsed, authorized, resolved and classified again before any connection. Only `http://`
//! is carried: a TLS transport needs a reviewed production dependency (see the WAS-02.t4 PR).

use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use super::HostError;
use super::bindings::CallControl;
use super::grant::GrantSet;
use super::http::{Destination, redirect_target};
use super::limits::HostLimits;
use super::net::{Reach, classify, reach};

const MAX_REDIRECTS: usize = 5;
const MAX_ADDRESSES: usize = 16;
const IO_SLICE: Duration = Duration::from_millis(50);
const READ_CHUNK: usize = 16 * 1024;
const METHODS: [&str; 7] = ["GET", "HEAD", "POST", "PUT", "DELETE", "PATCH", "OPTIONS"];
/// Framing and hop-by-hop headers belong to the gate, never to the guest.
const RESERVED_HEADERS: [&str; 10] = [
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "upgrade",
    "te",
    "trailer",
    "keep-alive",
    "expect",
    "proxy-authorization",
];

/// Name resolution, the one place a name becomes an address.
pub trait Resolver: Send + Sync {
    fn resolve(&self, host: &str) -> io::Result<Vec<IpAddr>>;
}

pub struct SystemResolver;

impl Resolver for SystemResolver {
    fn resolve(&self, host: &str) -> io::Result<Vec<IpAddr>> {
        Ok((host, 0)
            .to_socket_addrs()?
            .map(|address| address.ip())
            .collect())
    }
}

/// The only way the gate opens a connection. It receives an address the gate validated, never
/// a name, so a connector cannot re-resolve.
pub trait Connector: Send + Sync {
    fn connect(&self, address: SocketAddr, timeout: Duration) -> io::Result<TcpStream>;
}

pub struct TcpConnector;

impl Connector for TcpConnector {
    fn connect(&self, address: SocketAddr, timeout: Duration) -> io::Result<TcpStream> {
        TcpStream::connect_timeout(&address, timeout)
    }
}

/// What one hop sends; the destination was authorized and its address validated.
struct Hop<'a> {
    address: SocketAddr,
    destination: &'a Destination,
    method: &'a str,
    headers: &'a [(String, String)],
    body: &'a [u8],
}

#[derive(Clone, Debug, Default)]
pub struct HttpRequest {
    pub url: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// Performs one guest request. Every failure is a `host-error`; the reason stays host-side.
pub fn fetch(
    resolver: &dyn Resolver,
    connector: &dyn Connector,
    grants: &GrantSet,
    limits: &HostLimits,
    request: HttpRequest,
    control: &CallControl,
) -> Result<HttpResponse, HostError> {
    let header_bytes = request
        .headers
        .iter()
        .try_fold(0usize, |total, (name, value)| {
            total.checked_add(name.len())?.checked_add(value.len())
        })
        .unwrap_or(usize::MAX);
    limits.check_request(&[
        request.url.len(),
        request.method.len(),
        header_bytes,
        request.body.len(),
    ])?;
    let mut method = request.method.to_ascii_uppercase();
    if !METHODS.contains(&method.as_str()) || method != request.method {
        return Err(HostError::Denied);
    }
    validate_headers(&request.headers)?;
    let mut headers = request.headers;
    let mut body = request.body;
    let mut url = request.url;
    for hops in 0..=MAX_REDIRECTS {
        stop_check(control)?;
        let destination = Destination::parse(&url).map_err(|_| HostError::Denied)?;
        let literal_granted = grants.authorize_http(&destination)?;
        let address = validated_address(resolver, &destination, literal_granted)?;
        let hop = Hop {
            address,
            destination: &destination,
            method: &method,
            headers: &headers,
            body: &body,
        };
        let response = exchange(connector, &hop, limits, control)?;
        let location = header(&response.headers, "location");
        let redirect = matches!(response.status, 301 | 302 | 303 | 307 | 308) && location.is_some();
        let Some(location) = location.filter(|_| redirect) else {
            return Ok(response);
        };
        if hops == MAX_REDIRECTS {
            return Err(HostError::Denied);
        }
        let next = redirect_target(&destination, location).map_err(|_| HostError::Denied)?;
        if (response.status == 303 && method != "HEAD")
            || (matches!(response.status, 301 | 302) && method == "POST")
        {
            method = "GET".to_owned();
            body.clear();
        }
        if next.host() != destination.host() || next.port() != destination.port() {
            headers.retain(|(name, _)| {
                !["authorization", "cookie"].contains(&name.to_ascii_lowercase().as_str())
            });
        }
        url = next.to_string();
    }
    Err(HostError::Denied)
}

fn stop_check(control: &CallControl) -> Result<(), HostError> {
    control.stop_reason().map_or(Ok(()), Err)
}

fn validate_headers(headers: &[(String, String)]) -> Result<(), HostError> {
    let token = |name: &str| {
        !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
    };
    let value = |value: &str| {
        value
            .bytes()
            .all(|b| b == b'\t' || (b >= 0x20 && b != 0x7f))
    };
    headers
        .iter()
        .all(|(name, v)| {
            token(name)
                && value(v)
                && !RESERVED_HEADERS.contains(&name.to_ascii_lowercase().as_str())
        })
        .then_some(())
        .ok_or(HostError::Denied)
}

/// Resolves once, classifies every answer and returns the address to connect to. One
/// forbidden answer denies the hop: a mixed answer set is never partly trusted.
fn validated_address(
    resolver: &dyn Resolver,
    destination: &Destination,
    literal_granted: bool,
) -> Result<SocketAddr, HostError> {
    let addresses = match destination.literal() {
        Some(ip) => vec![ip],
        None => resolver
            .resolve(destination.host())
            .map_err(|_| HostError::Denied)?,
    };
    if addresses.is_empty() || addresses.len() > MAX_ADDRESSES {
        return Err(HostError::Denied);
    }
    for ip in &addresses {
        match reach(classify(*ip)) {
            Reach::Open => {}
            Reach::GrantOnly if literal_granted && destination.literal() == Some(*ip) => {}
            Reach::GrantOnly | Reach::Never => return Err(HostError::Denied),
        }
    }
    Ok(SocketAddr::new(addresses[0], destination.port()))
}

fn io_error(control: &CallControl) -> HostError {
    control.stop_reason().unwrap_or(HostError::Denied)
}

fn exchange(
    connector: &dyn Connector,
    hop: &Hop<'_>,
    limits: &HostLimits,
    control: &CallControl,
) -> Result<HttpResponse, HostError> {
    let Hop {
        address,
        destination,
        method,
        headers,
        body,
    } = *hop;
    stop_check(control)?;
    let remaining = control.remaining();
    if remaining.is_zero() {
        return Err(HostError::Timeout);
    }
    // The validated address is the only thing connected to; the name is never looked up again.
    let mut stream = connector
        .connect(address, remaining)
        .map_err(|_| io_error(control))?;
    stream
        .set_read_timeout(Some(IO_SLICE))
        .and_then(|()| stream.set_write_timeout(Some(IO_SLICE)))
        .map_err(|_| HostError::Denied)?;
    let mut head = format!(
        "{method} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nUser-Agent: falcone-serverless\r\n",
        destination.path_and_query(),
        host_header(destination),
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    if !body.is_empty() || ["POST", "PUT", "PATCH"].contains(&method) {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("\r\n");
    write_all(&mut stream, head.as_bytes(), control)?;
    write_all(&mut stream, body, control)?;
    read_response(&mut stream, method, limits, control)
}

fn host_header(destination: &Destination) -> String {
    let host = match destination.literal() {
        Some(IpAddr::V6(_)) => format!("[{}]", destination.host()),
        _ => destination.host().to_owned(),
    };
    if destination.port() == 80 {
        host
    } else {
        format!("{host}:{}", destination.port())
    }
}

fn write_all(
    stream: &mut TcpStream,
    mut bytes: &[u8],
    control: &CallControl,
) -> Result<(), HostError> {
    while !bytes.is_empty() {
        stop_check(control)?;
        match stream.write(bytes) {
            Ok(0) => return Err(HostError::Denied),
            Ok(n) => bytes = &bytes[n..],
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(io_error(control)),
        }
    }
    Ok(())
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

fn read_response(
    stream: &mut TcpStream,
    method: &str,
    limits: &HostLimits,
    control: &CallControl,
) -> Result<HttpResponse, HostError> {
    let cap = limits.host_to_guest_bytes();
    let mut buffer: Vec<u8> = Vec::new();
    let mut fill = |buffer: &mut Vec<u8>| -> Result<bool, HostError> {
        loop {
            stop_check(control)?;
            let mut chunk = [0u8; READ_CHUNK];
            match stream.read(&mut chunk) {
                Ok(0) => return Ok(false),
                Ok(n) => {
                    buffer.extend_from_slice(&chunk[..n]);
                    return Ok(true);
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => return Err(io_error(control)),
            }
        }
    };
    let head_end = loop {
        if let Some(position) = find(&buffer, b"\r\n\r\n") {
            break position;
        }
        if buffer.len() > cap {
            return Err(HostError::LimitExceeded);
        }
        if !fill(&mut buffer)? {
            return Err(HostError::Denied);
        }
    };
    let head = std::str::from_utf8(&buffer[..head_end]).map_err(|_| HostError::Denied)?;
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status = status_line
        .strip_prefix("HTTP/1.")
        .and_then(|rest| rest.get(2..5))
        .and_then(|code| code.parse::<u16>().ok())
        .filter(|code| (100..=599).contains(code))
        .ok_or(HostError::Denied)?;
    let mut headers = Vec::new();
    let mut header_bytes = 0usize;
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(HostError::Denied)?;
        if name.is_empty() || name.contains([' ', '\t']) {
            return Err(HostError::Denied);
        }
        header_bytes = header_bytes.saturating_add(name.len() + value.len());
        headers.push((name.to_owned(), value.trim().to_owned()));
    }
    limits.check_response(header_bytes)?;
    let body_start = head_end + 4;
    let no_body = method == "HEAD" || status < 200 || status == 204 || status == 304;
    let chunked = header(&headers, "transfer-encoding")
        .is_some_and(|value| value.eq_ignore_ascii_case("chunked"));
    let length = match header(&headers, "content-length") {
        Some(value) => Some(value.parse::<usize>().map_err(|_| HostError::Denied)?),
        None => None,
    };
    let body_cap = cap.saturating_sub(header_bytes);
    let body = if no_body {
        Vec::new()
    } else if chunked {
        buffer.drain(..body_start);
        read_chunked(&mut buffer, body_cap, &mut fill)?
    } else if let Some(length) = length {
        limits.check_response(header_bytes.saturating_add(length))?;
        while buffer.len() - body_start < length {
            if !fill(&mut buffer)? {
                return Err(HostError::Denied);
            }
        }
        buffer[body_start..body_start + length].to_vec()
    } else {
        while fill(&mut buffer)? {
            if buffer.len() - body_start > body_cap {
                return Err(HostError::LimitExceeded);
            }
        }
        buffer[body_start..].to_vec()
    };
    limits.check_response(header_bytes.saturating_add(body.len()))?;
    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Longest chunk-size line (size plus extensions) or trailer line the gate buffers.
const MAX_FRAMING_LINE: usize = 1024;

/// Decodes a chunked body incrementally. Consumed bytes leave `buffer` at once, so it never
/// holds more than the chunk being read or one framing line, and the total framing the server
/// may send is bounded by `cap` as well: a stream of extensions or trailers cannot grow
/// memory or hold the call until its deadline.
fn read_chunked(
    buffer: &mut Vec<u8>,
    cap: usize,
    fill: &mut dyn FnMut(&mut Vec<u8>) -> Result<bool, HostError>,
) -> Result<Vec<u8>, HostError> {
    let mut body = Vec::new();
    let mut framing = 0usize;
    let mut take_line = |buffer: &mut Vec<u8>,
                         fill: &mut dyn FnMut(&mut Vec<u8>) -> Result<bool, HostError>|
     -> Result<Vec<u8>, HostError> {
        loop {
            if let Some(end) = find(buffer, b"\r\n") {
                framing = framing.saturating_add(end + 2);
                if framing > cap {
                    return Err(HostError::LimitExceeded);
                }
                let line = buffer[..end].to_vec();
                buffer.drain(..end + 2);
                return Ok(line);
            }
            if buffer.len() > MAX_FRAMING_LINE {
                return Err(HostError::LimitExceeded);
            }
            if !fill(buffer)? {
                return Err(HostError::Denied);
            }
        }
    };
    loop {
        let line = take_line(buffer, fill)?;
        if line.len() > MAX_FRAMING_LINE {
            return Err(HostError::LimitExceeded);
        }
        let text = std::str::from_utf8(&line).map_err(|_| HostError::Denied)?;
        let size_text = text.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_text, 16).map_err(|_| HostError::Denied)?;
        if size == 0 {
            // Trailers are not carried; the terminator is the blank line after them.
            while !take_line(buffer, fill)?.is_empty() {}
            return Ok(body);
        }
        if body.len().saturating_add(size) > cap {
            return Err(HostError::LimitExceeded);
        }
        while buffer.len() < size + 2 {
            if !fill(buffer)? {
                return Err(HostError::Denied);
            }
        }
        if &buffer[size..size + 2] != b"\r\n" {
            return Err(HostError::Denied);
        }
        body.extend_from_slice(&buffer[..size]);
        buffer.drain(..size + 2);
    }
}
