//! Legacy `main(params)` request/result envelope and invocation identity (MIG-01.t3
//! fixtures in docs/contracts/fixtures/legacy-envelope/, consumed in place).

use super::request::InvocationIdentity;
use super::types::{ContractError, Id, parse};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// `max-host-to-guest-bytes` in worker/wit/host.wit.
pub const MAX_LEGACY_BODY_BYTES: usize = 1_048_576;

/// Why a legacy request is rejected before invocation. These are the inventory's
/// fixture labels, not failure codes: they describe a malformed request and are
/// outside the `FailureCode` set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LegacyRejection {
    EnvelopeInvalid,
    BodyTooLarge,
    BodyNotJson,
}

impl LegacyRejection {
    pub fn label(self) -> &'static str {
        match self {
            Self::EnvelopeInvalid => "envelope-invalid",
            Self::BodyTooLarge => "body-too-large",
            Self::BodyNotJson => "body-not-json",
        }
    }
}

/// The flat OpenWhisk-style `params` object. Action parameters sit beside the
/// `__ow_*` parameters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LegacyParams {
    #[serde(rename = "__ow_method")]
    pub method: String,
    #[serde(rename = "__ow_headers")]
    pub headers: BTreeMap<String, String>,
    #[serde(rename = "__ow_path")]
    pub path: String,
    #[serde(rename = "__ow_query")]
    pub query: BTreeMap<String, String>,
    #[serde(rename = "__ow_body", default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(flatten)]
    pub action_params: Map<String, Value>,
}

impl LegacyParams {
    /// Admits a legacy request or names why it is rejected before invocation.
    pub fn admit(value: &Value) -> Result<Self, LegacyRejection> {
        let params: Self = parse(value).map_err(|_| LegacyRejection::EnvelopeInvalid)?;
        if params
            .action_params
            .keys()
            .any(|key| key.starts_with("__ow_"))
        {
            return Err(LegacyRejection::EnvelopeInvalid);
        }
        if let Some(body) = &params.body {
            if body.len() > MAX_LEGACY_BODY_BYTES {
                return Err(LegacyRejection::BodyTooLarge);
            }
            if params.is_json_request() && serde_json::from_str::<Value>(body).is_err() {
                return Err(LegacyRejection::BodyNotJson);
            }
        }
        Ok(params)
    }

    fn is_json_request(&self) -> bool {
        self.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("content-type")
                && value
                    .split(';')
                    .next()
                    .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"))
        })
    }
}

/// The handler result `{statusCode, headers?, body?}`; a 304 has no body.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyResult {
    #[serde(rename = "statusCode")]
    pub status_code: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

impl LegacyResult {
    pub fn from_value(value: &Value) -> Result<Self, ContractError> {
        let result: Self = parse(value)?;
        if !(100..=599).contains(&result.status_code) {
            return Err(ContractError::new("v1: statusCode must be an HTTP status"));
        }
        if result.status_code == 304 && result.body.is_some() {
            return Err(ContractError::new("v1: a 304 result has no body"));
        }
        Ok(result)
    }
}

/// `invocation.headers` of a legacy fixture.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyHeaders {
    #[serde(rename = "X-Correlation-Id")]
    pub correlation_id: Id,
    #[serde(rename = "Idempotency-Key")]
    pub idempotency_key: Id,
}

/// `invocation.identity` of a legacy fixture, in the WIT function names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyIdentity {
    #[serde(rename = "invocation-key")]
    pub invocation_key: Id,
    #[serde(rename = "attempt-id")]
    pub attempt_id: Id,
    #[serde(rename = "idempotency-key")]
    pub idempotency_key: Id,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyInvocation {
    pub headers: LegacyHeaders,
    pub identity: LegacyIdentity,
}

impl LegacyInvocation {
    /// The v1 identity. The idempotency key travels in the header and in the identity,
    /// and the two must agree.
    pub fn identity(&self) -> Result<InvocationIdentity, ContractError> {
        if self.headers.idempotency_key != self.identity.idempotency_key {
            return Err(ContractError::new(
                "v1: Idempotency-Key header differs from the idempotency-key identity",
            ));
        }
        Ok(InvocationIdentity {
            invocation_key: self.identity.invocation_key.clone(),
            attempt_id: self.identity.attempt_id.clone(),
            idempotency_key: self.identity.idempotency_key.clone(),
            correlation_id: self.headers.correlation_id.clone(),
        })
    }
}
