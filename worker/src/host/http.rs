//! Pure HTTP destination handling for the egress gate: strict URL parsing and redirect
//! resolution. No name is resolved and no socket is opened here.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

const MAX_URL_BYTES: usize = 8192;

/// A rejected URL; the reason is deliberately not exposed to the guest.
#[derive(Debug, Eq, PartialEq)]
pub struct DestinationError;

#[derive(Debug, Eq, PartialEq)]
pub enum RedirectError {
    Invalid,
}

/// A validated `http://` destination. Userinfo, fragments, IDNA, numeric shorthand hosts
/// (`127.1`, `0x7f.1`, `2130706433`) and control characters are rejected rather than
/// normalized, so the allow-list sees exactly the host that would be connected to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Destination {
    host: String,
    port: u16,
    explicit_port: Option<u16>,
    path_and_query: String,
    literal: Option<IpAddr>,
}

impl Destination {
    pub fn parse(url: &str) -> Result<Self, DestinationError> {
        if url.len() > MAX_URL_BYTES || !url.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(DestinationError);
        }
        let rest = url
            .get(..7)
            .filter(|scheme| scheme.eq_ignore_ascii_case("http://"))
            .map(|_| &url[7..])
            .ok_or(DestinationError)?;
        let rest = rest.split('#').next().unwrap_or_default();
        let end = rest.find(['/', '?']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(end);
        if authority.contains('@') {
            return Err(DestinationError);
        }
        let path_and_query = match tail {
            "" => "/".to_owned(),
            _ if tail.starts_with('?') => format!("/{tail}"),
            _ => tail.to_owned(),
        };
        let (host, port_text) = split_authority(authority)?;
        let explicit_port = port_text.map(parse_port).transpose()?;
        let (host, literal) = parse_host(host, authority.starts_with('['))?;
        Ok(Self {
            host,
            port: explicit_port.unwrap_or(80),
            explicit_port,
            path_and_query,
            literal,
        })
    }

    /// Lowercase host without a trailing dot; an IP literal in canonical text form.
    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn explicit_port(&self) -> Option<u16> {
        self.explicit_port
    }

    pub fn path_and_query(&self) -> &str {
        &self.path_and_query
    }

    /// The IP address when the host is a literal; such a host is never resolved.
    pub fn literal(&self) -> Option<IpAddr> {
        self.literal
    }

    fn authority(&self) -> String {
        match self.literal {
            Some(IpAddr::V6(_)) => format!("[{}]:{}", self.host, self.port),
            _ => format!("{}:{}", self.host, self.port),
        }
    }
}

impl fmt::Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "http://{}{}", self.authority(), self.path_and_query)
    }
}

fn split_authority(authority: &str) -> Result<(&str, Option<&str>), DestinationError> {
    if let Some(inner) = authority.strip_prefix('[') {
        let (host, after) = inner.split_once(']').ok_or(DestinationError)?;
        return match after {
            "" => Ok((host, None)),
            _ => after
                .strip_prefix(':')
                .map(|port| (host, Some(port)))
                .ok_or(DestinationError),
        };
    }
    Ok(match authority.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    })
}

fn parse_port(text: &str) -> Result<u16, DestinationError> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(DestinationError);
    }
    text.parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or(DestinationError)
}

fn parse_host(host: &str, bracketed: bool) -> Result<(String, Option<IpAddr>), DestinationError> {
    if bracketed {
        let ip: Ipv6Addr = host.parse().map_err(|_| DestinationError)?;
        return Ok((ip.to_string(), Some(IpAddr::V6(ip))));
    }
    let host = host.to_ascii_lowercase();
    let host = host.strip_suffix('.').unwrap_or(&host);
    if host.is_empty() || host.contains(':') {
        return Err(DestinationError);
    }
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        return Ok((ip.to_string(), Some(IpAddr::V4(ip))));
    }
    let name_chars = host
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'.' | b'_'));
    let last = host.rsplit('.').next().unwrap_or_default();
    let numeric_shorthand = last.bytes().all(|b| b.is_ascii_digit()) || last.starts_with("0x");
    if !name_chars || numeric_shorthand || host.split('.').any(str::is_empty) {
        return Err(DestinationError);
    }
    Ok((host.to_owned(), None))
}

/// Resolves a `Location` value against the URL that returned it. Only `http://` targets are
/// representable; anything else is invalid. The result is re-validated by the caller like a
/// first request.
pub fn redirect_target(
    current: &Destination,
    location: &str,
) -> Result<Destination, RedirectError> {
    if location.is_empty() || !location.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(RedirectError::Invalid);
    }
    let location = location.split('#').next().unwrap_or_default();
    let url = if location.starts_with("//") {
        format!("http:{location}")
    } else if location.starts_with('/') {
        format!(
            "http://{}{}",
            current.authority(),
            remove_dot_segments(location)
        )
    } else if has_scheme(location) {
        location.to_owned()
    } else if location.starts_with('?') {
        let path = current.path_and_query.split('?').next().unwrap_or("/");
        format!("http://{}{path}{location}", current.authority())
    } else {
        let path = current.path_and_query.split('?').next().unwrap_or("/");
        let directory = &path[..=path.rfind('/').unwrap_or(0)];
        format!(
            "http://{}{}",
            current.authority(),
            remove_dot_segments(&format!("{directory}{location}"))
        )
    };
    Destination::parse(&url).map_err(|_| RedirectError::Invalid)
}

fn has_scheme(location: &str) -> bool {
    location.split_once(':').is_some_and(|(scheme, _)| {
        scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
    })
}

/// RFC 3986 section 5.2.4 on the path part; the query is carried through untouched.
fn remove_dot_segments(path_and_query: &str) -> String {
    let (path, query) = match path_and_query.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (path_and_query, None),
    };
    let mut out: Vec<&str> = Vec::new();
    for segment in path.split('/').skip(1) {
        match segment {
            "." => {}
            ".." => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    let mut result = format!("/{}", out.join("/"));
    if path.ends_with("/.") || path.ends_with("/..") {
        result.push('/');
    }
    if let Some(query) = query {
        result.push('?');
        result.push_str(query);
    }
    result
}
