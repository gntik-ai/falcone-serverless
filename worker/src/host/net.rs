//! Host-layer destination gate while ADR 0005 remains Proposed.
use super::{BUFFER_BYTES, Bindings, Context, HostError, Pair};
use std::net::{IpAddr, Ipv4Addr};
use std::{
    net::{SocketAddr, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};
use wasmtime::component::{ComponentType, Lift, Lower};

#[derive(Debug)]
pub struct Request {
    pub url: String,
    pub method: String,
    pub headers: Vec<Pair>,
    pub body: Vec<u8>,
}

#[derive(Debug, ComponentType, Lift, Lower)]
#[component(record)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<Pair>,
    pub body: Vec<u8>,
}

/// Created only by this gate, with a numeric peer and one-hop transport contract.
pub struct PinnedRequest {
    address: SocketAddr,
    authority: String,
    tls: bool,
    path: String,
    method: String,
    headers: Vec<Pair>,
    body: Vec<u8>,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
}
impl PinnedRequest {
    pub fn address(&self) -> SocketAddr {
        self.address
    }
    pub fn authority(&self) -> &str {
        &self.authority
    }
    pub fn tls(&self) -> bool {
        self.tls
    }
    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn method(&self) -> &str {
        &self.method
    }
    pub fn headers(&self) -> &[Pair] {
        &self.headers
    }
    pub fn body(&self) -> &[u8] {
        &self.body
    }
    pub fn deadline(&self) -> Instant {
        self.deadline
    }
    pub fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
    pub fn connect(&self) -> Result<TcpStream, HostError> {
        check_call(self.deadline, &self.cancelled)?;
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        let stream =
            TcpStream::connect_timeout(&self.address, remaining).map_err(|_| HostError::Denied)?;
        stream
            .set_read_timeout(Some(remaining))
            .map_err(|_| HostError::Denied)?;
        stream
            .set_write_timeout(Some(remaining))
            .map_err(|_| HostError::Denied)?;
        Ok(stream)
    }
}

fn check_call(deadline: Instant, cancelled: &AtomicBool) -> Result<(), HostError> {
    if cancelled.load(Ordering::Acquire) {
        Err(HostError::Cancelled)
    } else if Instant::now() >= deadline {
        Err(HostError::Timeout)
    } else {
        Ok(())
    }
}

fn size(headers: &[Pair], body: &[u8], initial: usize) -> Result<(), HostError> {
    let mut total = initial
        .checked_add(8)
        .and_then(|n| n.checked_add(body.len()))
        .ok_or(HostError::LimitExceeded)?;
    if headers.len() > BUFFER_BYTES / 8 || total > BUFFER_BYTES {
        return Err(HostError::LimitExceeded);
    }
    for pair in headers {
        total = total
            .checked_add(8)
            .and_then(|n| n.checked_add(pair.name.len()))
            .and_then(|n| n.checked_add(pair.value.len()))
            .filter(|n| *n <= BUFFER_BYTES)
            .ok_or(HostError::LimitExceeded)?;
    }
    Ok(())
}

struct Url {
    host: String,
    port: u16,
    path: String,
    tls: bool,
}
fn parse_url(url: &str) -> Result<Url, HostError> {
    if url
        .bytes()
        .any(|byte| byte <= 32 || byte >= 127 || byte == b'\\')
    {
        return Err(HostError::Denied);
    }
    let (scheme, rest) = url.split_once("://").ok_or(HostError::Denied)?;
    let tls = match scheme {
        "http" => false,
        "https" => true,
        _ => return Err(HostError::Denied),
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    if authority.contains(['@', '%']) || authority.is_empty() {
        return Err(HostError::Denied);
    }
    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let (host, suffix) = bracketed.split_once(']').ok_or(HostError::Denied)?;
        if !matches!(host.parse::<IpAddr>(), Ok(IpAddr::V6(_))) {
            return Err(HostError::Denied);
        }
        (host, suffix)
    } else {
        let (host, port) = authority
            .split_once(':')
            .map_or((authority, ""), |(host, port)| {
                (host, &authority[host.len()..host.len() + port.len() + 1])
            });
        if host.is_empty()
            || host.len() > 253
            || host.split('.').any(|label| {
                label.is_empty()
                    || label.len() > 63
                    || label.starts_with('-')
                    || label.ends_with('-')
                    || !label
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            })
        {
            return Err(HostError::Denied);
        }
        (host, port)
    };
    let port = if port.is_empty() {
        if tls { 443 } else { 80 }
    } else {
        port.strip_prefix(':')
            .and_then(|value| value.parse::<u16>().ok())
            .filter(|port| *port != 0)
            .ok_or(HostError::Denied)?
    };
    let path = rest[end..].split('#').next().unwrap_or("");
    let path = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    };
    Ok(Url {
        host: host.to_ascii_lowercase(),
        port,
        path,
        tls,
    })
}

pub(super) fn request(
    binding: &dyn Bindings,
    context: &Context,
    mut request: Request,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
) -> Result<Response, HostError> {
    if request.method.is_empty() || !request.method.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return Err(HostError::Denied);
    }
    for header in &request.headers {
        if header.name.is_empty()
            || !header
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || header.value.bytes().any(|byte| byte < 32 || byte == 127)
            || [
                "host",
                "connection",
                "content-length",
                "transfer-encoding",
                "proxy-authorization",
            ]
            .iter()
            .any(|name| header.name.eq_ignore_ascii_case(name))
        {
            return Err(HostError::Denied);
        }
    }
    for hop in 0..=5 {
        check_call(deadline, &cancelled)?;
        size(
            &request.headers,
            &request.body,
            request
                .url
                .len()
                .checked_add(request.method.len())
                .and_then(|n| n.checked_add(8))
                .ok_or(HostError::LimitExceeded)?,
        )?;
        let url = parse_url(&request.url)?;
        if !context
            .grants
            .hosts
            .iter()
            .any(|host| !host.contains('*') && host.eq_ignore_ascii_case(&url.host))
        {
            return Err(HostError::Denied);
        }
        let addresses = match url.host.parse::<IpAddr>() {
            Ok(ip) => vec![ip],
            Err(_) => binding.resolve(&url.host)?,
        };
        if addresses.is_empty() || addresses.iter().any(|address| !public_address(*address)) {
            return Err(HostError::Denied);
        }
        check_call(deadline, &cancelled)?;
        let pinned = PinnedRequest {
            address: SocketAddr::new(addresses[0], url.port),
            authority: url.host.clone(),
            tls: url.tls,
            path: url.path.clone(),
            method: request.method.clone(),
            headers: request.headers.clone(),
            body: request.body.clone(),
            deadline,
            cancelled: cancelled.clone(),
        };
        let response = binding.http(context, &pinned)?;
        check_call(deadline, &cancelled)?;
        size(&response.headers, &response.body, 2)?;
        if ![301, 302, 303, 307, 308].contains(&response.status) {
            return Ok(response);
        }
        if hop == 5 {
            return Err(HostError::LimitExceeded);
        }
        let mut locations = response
            .headers
            .iter()
            .filter(|pair| pair.name.eq_ignore_ascii_case("location"));
        let location = &locations.next().ok_or(HostError::Denied)?.value;
        if locations.next().is_some() || location.is_empty() {
            return Err(HostError::Denied);
        }
        request.url = if location.starts_with("//") {
            format!("{}:{location}", if url.tls { "https" } else { "http" })
        } else if location.starts_with('/') {
            let host = if url.host.contains(':') {
                format!("[{}]", url.host)
            } else {
                url.host
            };
            format!(
                "{}://{host}:{}{location}",
                if url.tls { "https" } else { "http" },
                url.port
            )
        } else if location.contains("://") {
            location.clone()
        } else {
            return Err(HostError::Denied);
        };
        if response.status == 303 {
            request.method = "GET".into();
            request.body.clear();
        }
    }
    Err(HostError::LimitExceeded)
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_multicast()
        || a == 0
        || a >= 240
        || (a == 100 && (64..128).contains(&b))
        || (a == 192 && b == 0 && (c == 0 || c == 2))
        || (a == 192 && b == 88 && c == 99)
        || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
        || (a == 203 && b == 0 && c == 113))
}

pub fn public_address(ip: IpAddr) -> bool {
    let IpAddr::V6(ip) = ip else {
        return public_v4(match ip {
            IpAddr::V4(ip) => ip,
            _ => unreachable!(),
        });
    };
    if let Some(ip) = ip.to_ipv4_mapped() {
        return public_v4(ip);
    }
    let segments = ip.segments();
    // RFC 8215 local-use /48: the translator may reach any private network.
    if segments[..3] == [0x64, 0xff9b, 1] {
        return false;
    }
    // RFC 6052 well-known /96: classify the embedded IPv4 destination.
    if segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        let bytes = ip.octets();
        return public_v4(Ipv4Addr::new(bytes[12], bytes[13], bytes[14], bytes[15]));
    }
    // Accept global unicast only, excluding documentation, transition and
    // protocol-assignment space. This also denies ULA, loopback and link-local.
    segments[0] & 0xe000 == 0x2000
        && segments[0] != 0x2002
        && !(segments[0] == 0x2001 && (segments[1] < 0x200 || segments[1] == 0xdb8))
        && !(segments[0] == 0x3fff && segments[1] & 0xf000 == 0)
}
