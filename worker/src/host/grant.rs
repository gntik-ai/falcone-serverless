//! Verified capability grants and the scope checks derived from them.
//!
//! The only authority source is the manifest `grants` array (docs/contracts/revision-runtime-manifest.md).
//! Guest arguments are matched against it and never widen it. Matching is exact: partial
//! wildcard syntax confers no authority, and a bare `*` is rejected when the grant is loaded.

use std::net::IpAddr;

use serde_json::Value;

use super::HostError;
use super::http::Destination;

pub const STORAGE: &str = "falcone:host/storage@0.1.0";
pub const HTTP: &str = "falcone:host/http@0.1.0";
pub const EVENTS: &str = "falcone:host/events@0.1.0";
pub const WORKFLOWS: &str = "falcone:host/workflows@0.1.0";

/// Interfaces that carry no external authority: they only read verified invocation context
/// or write to host-side sinks scoped by it. No grant family covers them.
const BASELINE: [&str; 4] = [
    "falcone:host/logging@0.1.0",
    "falcone:host/cancellation@0.1.0",
    "falcone:host/progress@0.1.0",
    "falcone:host/invocation-context@0.1.0",
];

const MAX_KEY_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportVerdict {
    /// A verified grant covers the interface.
    Granted,
    /// A declared interface without authority of its own.
    Baseline,
    /// A declared interface that no grant covers: instantiation must fail.
    Ungranted,
    /// Not a `falcone:host@0.1.0` interface (WASI, other versions, ad-hoc names).
    Undeclared,
}

#[derive(Debug, Eq, PartialEq)]
pub struct GrantError(pub &'static str);

impl std::fmt::Display for GrantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid grant: {}", self.0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct HttpEntry {
    host: String,
    port: Option<u16>,
    literal: Option<IpAddr>,
}

/// The union of the verified grants of one invocation.
#[derive(Clone, Debug, Default)]
pub struct GrantSet {
    storage: Option<Vec<String>>,
    http: Option<Vec<HttpEntry>>,
    events: Option<Vec<String>>,
    workflows: Option<Vec<String>>,
}

impl GrantSet {
    /// Parses and validates the manifest `grants` array. A malformed grant is an error, never
    /// a narrower or wider grant.
    pub fn from_manifest(grants: &Value) -> Result<Self, GrantError> {
        let grants = grants
            .as_array()
            .ok_or(GrantError("grants must be an array"))?;
        let mut set = Self::default();
        for grant in grants {
            let object = grant
                .as_object()
                .ok_or(GrantError("grant must be an object"))?;
            if object.keys().any(|key| {
                !["family", "interface", "scope", "secretRef", "guestGrantId"]
                    .contains(&key.as_str())
            }) {
                return Err(GrantError("unknown grant property"));
            }
            let family = object.get("family").and_then(Value::as_str);
            let interface = object.get("interface").and_then(Value::as_str);
            let (interface_expected, field) = match family {
                Some("storage") => (STORAGE, "prefixes"),
                Some("http") => (HTTP, "hosts"),
                Some("event") => (EVENTS, "topics"),
                Some("workflow") => (WORKFLOWS, "targets"),
                _ => return Err(GrantError("unknown grant family")),
            };
            if interface != Some(interface_expected) {
                return Err(GrantError("interface does not match the family"));
            }
            let scope = object
                .get("scope")
                .and_then(Value::as_object)
                .ok_or(GrantError("scope must be an object"))?;
            if scope.len() != 1 {
                return Err(GrantError("scope must hold exactly the family field"));
            }
            let entries = scope
                .get(field)
                .and_then(Value::as_array)
                .filter(|entries| !entries.is_empty())
                .ok_or(GrantError("scope field must be a non-empty list"))?;
            let entries = entries
                .iter()
                .map(|entry| entry.as_str().filter(|e| !e.is_empty() && *e != "*"))
                .collect::<Option<Vec<_>>>()
                .ok_or(GrantError(
                    "scope entries must be non-empty strings, never *",
                ))?;
            match family {
                Some("storage") => {
                    let prefixes = entries
                        .iter()
                        .map(|prefix| normalized_prefix(prefix))
                        .collect::<Option<Vec<_>>>()
                        .ok_or(GrantError(
                            "storage prefix is not a canonical relative path",
                        ))?;
                    set.storage.get_or_insert_with(Vec::new).extend(prefixes);
                }
                Some("http") => {
                    let parsed = entries
                        .iter()
                        .map(|entry| http_entry(entry))
                        .collect::<Option<Vec<_>>>()
                        .ok_or(GrantError("http host entry is not host or host:port"))?;
                    set.http.get_or_insert_with(Vec::new).extend(parsed);
                }
                Some("event") => set
                    .events
                    .get_or_insert_with(Vec::new)
                    .extend(entries.iter().map(|e| (*e).to_owned())),
                _ => set
                    .workflows
                    .get_or_insert_with(Vec::new)
                    .extend(entries.iter().map(|e| (*e).to_owned())),
            }
        }
        Ok(set)
    }

    /// True when a verified grant covers the interface (baseline interfaces are not covered
    /// by grants).
    pub fn covers_interface(&self, interface: &str) -> bool {
        match interface {
            STORAGE => self.storage.is_some(),
            HTTP => self.http.is_some(),
            EVENTS => self.events.is_some(),
            WORKFLOWS => self.workflows.is_some(),
            _ => false,
        }
    }

    pub fn classify_import(&self, name: &str) -> ImportVerdict {
        if [STORAGE, HTTP, EVENTS, WORKFLOWS].contains(&name) {
            if self.covers_interface(name) {
                ImportVerdict::Granted
            } else {
                ImportVerdict::Ungranted
            }
        } else if BASELINE.contains(&name) {
            ImportVerdict::Baseline
        } else {
            ImportVerdict::Undeclared
        }
    }

    /// A key must be a canonical relative path equal to, or below, a granted prefix. Prefixes
    /// are segment-aware: `ws1` never covers `ws10`.
    pub fn authorize_storage_key(&self, key: &str) -> Result<(), HostError> {
        let prefixes = self.storage.as_deref().unwrap_or_default();
        if !canonical_key(key) {
            return Err(HostError::Denied);
        }
        let inside = prefixes.iter().any(|prefix| {
            key == prefix
                || key
                    .strip_prefix(prefix.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
        });
        inside.then_some(()).ok_or(HostError::Denied)
    }

    pub fn authorize_event_topic(&self, topic: &str) -> Result<(), HostError> {
        exact(self.events.as_deref(), topic)
    }

    pub fn authorize_workflow_target(&self, target: &str) -> Result<(), HostError> {
        exact(self.workflows.as_deref(), target)
    }

    /// `Ok(true)` when the destination is allow-listed by an IP-literal entry, which is the
    /// only grant that may reach a loopback or private address; `Ok(false)` for a name entry.
    pub fn authorize_http(&self, destination: &Destination) -> Result<bool, HostError> {
        let entries = self.http.as_deref().unwrap_or_default();
        let matched = entries.iter().find(|entry| {
            entry.host == destination.host()
                && entry.port.unwrap_or(80) == destination.port()
                && entry.literal == destination.literal()
        });
        matched
            .map(|entry| entry.literal.is_some())
            .ok_or(HostError::Denied)
    }
}

fn exact(entries: Option<&[String]>, value: &str) -> Result<(), HostError> {
    entries
        .is_some_and(|entries| entries.iter().any(|entry| entry == value))
        .then_some(())
        .ok_or(HostError::Denied)
}

fn disallowed_char(c: char) -> bool {
    c.is_control()
        || c == '\\'
        || matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
}

fn percent_traversal(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    ["%2e", "%2f", "%5c", "%00"]
        .iter()
        .any(|needle| lower.contains(needle))
}

/// A canonical relative key: no empty, `.` or `..` segment, no leading or trailing `/`, no
/// backslash, control, bidi or percent-encoded separator.
fn canonical_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_KEY_BYTES
        && !key.chars().any(disallowed_char)
        && !percent_traversal(key)
        && key
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

fn normalized_prefix(prefix: &str) -> Option<String> {
    let trimmed = prefix.strip_suffix('/').unwrap_or(prefix);
    canonical_key(trimmed).then(|| trimmed.to_owned())
}

/// An entry is `host` or `host:port` only: no scheme, path, query, userinfo or wildcard.
fn http_entry(entry: &str) -> Option<HttpEntry> {
    if entry.contains(['/', '?', '#', '@', '*']) {
        return None;
    }
    let destination = Destination::parse(&format!("http://{entry}/")).ok()?;
    Some(HttpEntry {
        host: destination.host().to_owned(),
        port: destination.explicit_port(),
        literal: destination.literal(),
    })
}
