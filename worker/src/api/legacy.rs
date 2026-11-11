//! The legacy `main(params)` envelope: the result shape, the invocation identity records and
//! the three inputs rejected before invocation (`docs/contracts/falcone-contract-inventory.md`).

use super::{
    error::{ErrorBody, Malformed, Reason},
    from_bytes,
    request::ExecuteRequest,
    result::ExecutionResult,
    types::{Presence, Redacted, present},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{collections::BTreeMap, fmt};

/// `max-host-to-guest-bytes` of `worker/wit/host.wit`: the largest `__ow_body`.
pub const LEGACY_BODY_LIMIT_BYTES: usize = 1_048_576;

/// The legacy result a function returns: an HTTP-like status, optional headers and body.
/// Headers and body are absent when the function sets none, never `null`.
#[derive(Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyResponse {
    #[serde(rename = "statusCode")]
    pub status_code: u16,
    #[serde(
        default = "Presence::absent",
        deserialize_with = "present",
        skip_serializing_if = "Presence::is_absent"
    )]
    pub headers: Presence<BTreeMap<String, String>>,
    #[serde(
        default = "Presence::absent",
        deserialize_with = "present",
        skip_serializing_if = "Presence::is_absent"
    )]
    pub body: Presence<Value>,
}

impl LegacyResponse {
    pub(super) fn check(&self) -> Result<(), Malformed> {
        self.headers.value()?;
        self.body.value()?;
        if (100..=599).contains(&self.status_code) {
            Ok(())
        } else {
            Err(Malformed::Rule("status_code"))
        }
    }
}

impl fmt::Debug for LegacyResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LegacyResponse")
            .field("status_code", &self.status_code)
            .finish_non_exhaustive()
    }
}

/// The invocation identity of a legacy fixture; none of it is a credential.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationIdentity {
    #[serde(rename = "invocation-key")]
    pub invocation_key: String,
    #[serde(rename = "attempt-id")]
    pub attempt_id: String,
    #[serde(rename = "idempotency-key")]
    pub idempotency_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Invocation {
    pub headers: BTreeMap<String, String>,
    pub identity: InvocationIdentity,
}

/// A valid legacy fixture: the input, the dependency state that reaches its path and the
/// result envelope the reference function produced.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyCase {
    pub path: String,
    #[serde(rename = "decidedBy")]
    pub decided_by: String,
    pub description: String,
    pub invocation: Invocation,
    pub params: Redacted<Value>,
    #[serde(rename = "dependencyState")]
    pub dependency_state: Redacted<Value>,
    pub expected: LegacyResponse,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InvalidExpectation {
    pub invoked: bool,
    pub rejection: String,
}

/// An invalid legacy fixture, expected to be rejected before invocation.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyInvalid {
    pub case: String,
    pub description: String,
    pub invocation: Invocation,
    pub params: Redacted<Value>,
    pub expected: InvalidExpectation,
}

pub fn parse_legacy_case(bytes: &[u8]) -> Result<LegacyCase, Malformed> {
    let case: LegacyCase = from_bytes(bytes)?;
    case.expected.check()?;
    Ok(case)
}

pub fn parse_legacy_invalid(bytes: &[u8]) -> Result<LegacyInvalid, Malformed> {
    from_bytes(bytes)
}

/// Inputs rejected before invocation, each with its documented code and reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LegacyRejection {
    MissingPath,
    NonJsonBody,
    BodyTooLarge,
}

impl LegacyRejection {
    pub fn reason(self) -> Reason {
        match self {
            Self::MissingPath => Reason::MissingPath,
            Self::NonJsonBody => Reason::NonJsonBody,
            Self::BodyTooLarge => Reason::BodyTooLarge,
        }
    }

    pub fn error(self) -> ErrorBody {
        ErrorBody::new(self.reason())
    }

    /// The non-retryable failure result for the rejected attempt.
    pub fn result(self, request: &ExecuteRequest) -> ExecutionResult {
        ExecutionResult::failure(request, self.reason())
    }
}

impl fmt::Display for LegacyRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "legacy input rejected before invocation: {}",
            self.error().code.as_str()
        )
    }
}

impl std::error::Error for LegacyRejection {}

fn json_content_type(params: &Map<String, Value>) -> bool {
    params
        .get("__ow_headers")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .filter_map(|(_, value)| value.as_str())
        .any(|value| {
            let media = value.split(';').next().unwrap_or("").trim();
            media.eq_ignore_ascii_case("application/json")
        })
}

/// Run `invoke` on `params` only when they are admissible. An oversized body is rejected
/// first; a web invocation (`__ow_method`) without `__ow_path` is malformed; a JSON request
/// whose `__ow_body` is not JSON is rejected. A rejected input never reaches `invoke`.
pub fn admit_legacy<R>(
    params: &Map<String, Value>,
    invoke: impl FnOnce(&Map<String, Value>) -> R,
) -> Result<R, LegacyRejection> {
    let body = params.get("__ow_body");
    if body
        .and_then(Value::as_str)
        .is_some_and(|text| text.len() > LEGACY_BODY_LIMIT_BYTES)
    {
        return Err(LegacyRejection::BodyTooLarge);
    }
    if params.contains_key("__ow_method") && !params.contains_key("__ow_path") {
        return Err(LegacyRejection::MissingPath);
    }
    if json_content_type(params) {
        let parsable = body
            .and_then(Value::as_str)
            .is_some_and(|text| serde_json::from_str::<Value>(text).is_ok());
        if body.is_some() && !parsable {
            return Err(LegacyRejection::NonJsonBody);
        }
    }
    Ok(invoke(params))
}
