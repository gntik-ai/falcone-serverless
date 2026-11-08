//! Invocation capability boundary. Callers supply only Falcone-verified scopes;
//! guest data never supplies a tenant, workspace, binding or grant.

pub mod execute;
pub mod net;
mod runtime;
pub use runtime::{BUFFER_BYTES, GUEST_MEMORY_BYTES, Host, instantiate, instantiate_component};
use wasmtime::component::{ComponentType, Lift, Lower};

pub fn aggregate_within_limit(parts: &[usize]) -> bool {
    parts
        .iter()
        .try_fold(0usize, |sum, part| sum.checked_add(*part))
        .is_some_and(|sum| sum <= BUFFER_BYTES)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ComponentType, Lift, Lower)]
#[component(variant)]
pub enum HostError {
    #[component(name = "denied")]
    Denied,
    #[component(name = "limit-exceeded")]
    LimitExceeded,
    #[component(name = "timeout")]
    Timeout,
    #[component(name = "cancelled")]
    Cancelled,
}

#[derive(Clone, Copy, Debug, ComponentType, Lift, Lower)]
#[component(enum)]
#[repr(u8)]
pub enum Level {
    #[component(name = "debug")]
    Debug,
    #[component(name = "info")]
    Info,
    #[component(name = "warn")]
    Warn,
    #[component(name = "error")]
    Error,
}

#[derive(Clone, Debug, ComponentType, Lift, Lower)]
#[component(record)]
pub struct Pair {
    pub name: String,
    pub value: String,
}

/// Trusted host context, never populated by a component argument.
#[derive(Clone, Debug, Default)]
pub struct Context {
    pub grants: Grants,
    pub tenant: String,
    pub workspace: String,
    pub invocation_key: String,
    pub attempt_id: String,
    pub idempotency_key: String,
}

#[derive(Debug)]
pub enum Operation {
    Publish {
        destination: String,
        payload: Vec<u8>,
    },
    Log {
        severity: Level,
        message: String,
        fields: Vec<Pair>,
    },
    Read {
        key: String,
        checkpoint: bool,
    },
    Write {
        key: String,
        checkpoint: bool,
        data: Vec<u8>,
    },
    Start {
        workflow: String,
        input: Vec<u8>,
    },
    Signal {
        execution: String,
        name: String,
        payload: Vec<u8>,
    },
    Http(net::Request),
    Progress {
        completed: u64,
        total: u64,
        message: String,
    },
    ProgressCheckpoint {
        key: String,
    },
}

pub enum Reply {
    Empty,
    Data(Vec<u8>),
    Execution(String),
    Http(net::Response),
}

/// The external platform boundary. Implementations retain credentials in the host.
pub trait Bindings: Send + Sync + 'static {
    fn call(&self, context: &Context, operation: Operation) -> Result<Reply, HostError>;
    fn resolve(&self, _host: &str) -> Result<Vec<std::net::IpAddr>, HostError> {
        Err(HostError::Denied)
    }
    /// One exchange with a validated peer. Never resolve again or follow redirects.
    /// Use PinnedRequest::connect for the connection; authority is for Host/TLS SNI.
    fn http(
        &self,
        _context: &Context,
        _request: &net::PinnedRequest,
    ) -> Result<net::Response, HostError> {
        Err(HostError::Denied)
    }
}

/// Concrete scopes from the verified revision manifest. Wildcards confer no authority.
#[derive(Clone, Debug, Default)]
pub struct Grants {
    pub prefixes: Vec<String>,
    pub hosts: Vec<String>,
    pub topics: Vec<String>,
    pub targets: Vec<String>,
}

fn relative_key(key: &str) -> bool {
    !key.is_empty()
        && !key.contains(['\\', '%', '\0'])
        && key
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

impl Grants {
    pub fn allows_key(&self, key: &str) -> bool {
        relative_key(key)
            && self.prefixes.iter().any(|prefix| {
                let path = prefix.strip_suffix('/').unwrap_or(prefix);
                relative_key(path)
                    && !path.contains('*')
                    // A slash-terminated grant covers descendants only.
                    && (key == prefix
                        || key
                            .strip_prefix(path)
                            .is_some_and(|tail| tail.starts_with('/')))
            })
    }
}

pub fn import_allowed(import: &str, grants: &Grants) -> bool {
    match import {
        "falcone:host/storage@0.2.0" => !grants.prefixes.is_empty(),
        "falcone:host/http@0.2.0" => !grants.hosts.is_empty(),
        "falcone:host/events@0.2.0" => !grants.topics.is_empty(),
        "falcone:host/workflows@0.2.0" => !grants.targets.is_empty(),
        "falcone:host/logging@0.2.0"
        | "falcone:host/cancellation@0.2.0"
        | "falcone:host/progress@0.2.0"
        | "falcone:host/invocation-context@0.2.0" => true,
        _ => false,
    }
}
