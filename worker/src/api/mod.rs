//! ctr-worker-execution-api v1: typed parsers for the execute, update, cancel and result
//! messages (`docs/contracts/worker-execution-api/`).
//!
//! The parsers enforce every rule of the published JSON Schemas with serde and explicit
//! checks. They never load or evaluate JSON Schema; schema conformance runs in tests only.
//!
//! Redaction by construction: no type here derives `Debug`, and no formatted error, `Debug` or
//! `Display` output carries a request value. Errors name the violated rule, never the input.
//!
//! Parsing is JSON only. Nothing in this module loads, compiles or executes a guest.

mod grants;
mod messages;
mod projection;
mod refusal;
mod request;
mod result;
mod service;
mod types;
mod wire;

pub use messages::{CancelMessage, UpdateMessage};
pub use projection::Projection;
pub use refusal::{Code, Reason, Refusal};
pub use request::ExecuteRequest;
pub use result::{ExecutionResult, HandlerResult, Pinned};
pub use service::WorkerCeilings;

use serde::Deserialize;
use std::fmt;
use types::{Id, Positive};

/// A message that violates its schema or a rule the schema cannot state.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Malformed {
    /// Not JSON.
    Syntax,
    /// JSON that does not match the schema.
    Shape,
    /// A cross-field rule, named by a fixed label.
    Constraint(&'static str),
}

fn constraint(rule: &'static str) -> Malformed {
    Malformed::Constraint(rule)
}

impl From<serde_json::Error> for Malformed {
    /// Only the category is kept: serde messages quote request values.
    fn from(error: serde_json::Error) -> Self {
        if error.is_data() {
            Self::Shape
        } else {
            Self::Syntax
        }
    }
}

impl fmt::Debug for Malformed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax => f.write_str("Malformed::Syntax"),
            Self::Shape => f.write_str("Malformed::Shape"),
            Self::Constraint(rule) => write!(f, "Malformed::Constraint({rule})"),
        }
    }
}

impl fmt::Display for Malformed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax => f.write_str("malformed message: not JSON"),
            Self::Shape => f.write_str("malformed message: does not match the schema"),
            Self::Constraint(rule) => write!(f, "malformed message: rule {rule} violated"),
        }
    }
}

/// The activation a refusal answers, kept so the caller can build the refusal result. Every
/// value in it is request-controlled and prints as a redaction marker.
#[derive(Clone, PartialEq)]
pub struct Attribution {
    pub activation_id: Id,
    pub attempt_id: Id,
    pub attempt_number: Positive,
    pub projection: Box<Projection>,
}
wire::redacted!(Attribution);

/// A request the worker refuses with a non-retryable failure; nothing is invoked.
#[derive(Clone, PartialEq)]
pub struct Refused {
    pub refusal: Refusal,
    /// Absent when the refusal came before the request could be read in full.
    pub attribution: Option<Attribution>,
}

impl Refused {
    fn new(refusal: Refusal, attribution: Option<Attribution>) -> Self {
        Self {
            refusal,
            attribution,
        }
    }
}

impl fmt::Debug for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let attribution = self.attribution.as_ref().map(|_| "<redacted>");
        write!(f, "Refused({:?}, {attribution:?})", self.refusal)
    }
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.refusal, f)
    }
}

/// Why an execute message did not become an invocable request.
#[derive(Clone, PartialEq)]
pub enum Rejection {
    Malformed(Malformed),
    Refused(Refused),
}

impl From<Malformed> for Rejection {
    fn from(error: Malformed) -> Self {
        Self::Malformed(error)
    }
}

impl fmt::Debug for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(error) => write!(f, "Rejection::Malformed({error:?})"),
            Self::Refused(refused) => write!(f, "Rejection::Refused({refused:?})"),
        }
    }
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(error) => fmt::Display::fmt(error, f),
            Self::Refused(refused) => fmt::Display::fmt(refused, f),
        }
    }
}

/// Parses an execute message and admits it to the Wasm worker.
///
/// A runtime mode other than `wasm`, including a value the schema does not know, is refused
/// with the non-retryable `runtime_mode_not_supported`. The legacy inputs without `__ow_path`,
/// with an oversized body or with a non-JSON body for a JSON content type are refused before
/// invocation. A returned request has passed every check.
pub fn parse_request(bytes: &[u8]) -> Result<ExecuteRequest, Rejection> {
    match serde_json::from_slice::<ExecuteRequest>(bytes) {
        Ok(request) => request.admit(),
        Err(error) => Err(match other_runtime_mode(bytes) {
            true => Rejection::Refused(Refused::new(
                Refusal::new(Code::RuntimeModeNotSupported, Reason::RuntimeModeNotWasm),
                None,
            )),
            false => Rejection::Malformed(error.into()),
        }),
    }
}

/// Whether the message names a runtime mode that is a string other than `wasm`.
fn other_runtime_mode(bytes: &[u8]) -> bool {
    #[derive(Deserialize)]
    struct Probe {
        projection: ProbeProjection,
    }
    #[derive(Deserialize)]
    struct ProbeProjection {
        runtime_mode: String,
    }
    serde_json::from_slice::<Probe>(bytes)
        .is_ok_and(|probe| probe.projection.runtime_mode != "wasm")
}

/// Parses a revision projection of either runtime mode, without admission.
pub fn parse_projection(bytes: &[u8]) -> Result<Projection, Malformed> {
    let projection: Projection = serde_json::from_slice(bytes)?;
    projection.check()?;
    Ok(projection)
}

pub fn parse_update(bytes: &[u8]) -> Result<UpdateMessage, Malformed> {
    let update: UpdateMessage = serde_json::from_slice(bytes)?;
    update.check()?;
    Ok(update)
}

pub fn parse_cancel(bytes: &[u8]) -> Result<CancelMessage, Malformed> {
    Ok(serde_json::from_slice(bytes)?)
}

/// Parses a result and checks it against the request (or projection) it answers: the identity
/// must describe the pinned revision, and `base_digest` must be present exactly when the
/// projection pins a base.
pub fn parse_result(bytes: &[u8], pinned: &impl Pinned) -> Result<ExecutionResult, Malformed> {
    let result: ExecutionResult = serde_json::from_slice(bytes)?;
    result.check(pinned)?;
    Ok(result)
}

#[cfg(test)]
mod tests;
