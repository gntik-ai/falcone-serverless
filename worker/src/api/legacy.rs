//! The legacy `main(params)` request and `{statusCode, headers?, body?}` result envelopes
//! (inventory v1.1), and the invocation identity that travels with every request.

use super::{
    error::{ApiError, MalformedReason, Mismatch, RequestReason},
    redact::{Redacted, present},
};
use serde::{Deserialize, Serialize, de::IgnoredAny};
use serde_json::{Map, Value};

/// `max-host-to-guest-bytes` in worker/wit/host.wit: the largest `__ow_body`.
pub const MAX_BODY_BYTES: usize = 1_048_576;

/// The `falcone:host/invocation-context` identity (worker/wit/host.wit).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationIdentity {
    #[serde(rename = "invocation-key")]
    pub invocation_key: Redacted<String>,
    #[serde(rename = "attempt-id")]
    pub attempt_id: Redacted<String>,
    #[serde(rename = "idempotency-key")]
    pub idempotency_key: Redacted<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationHeaders {
    #[serde(rename = "X-Correlation-Id")]
    pub correlation_id: Redacted<String>,
    #[serde(rename = "Idempotency-Key")]
    pub idempotency_key: Redacted<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Invocation {
    pub headers: InvocationHeaders,
    pub identity: InvocationIdentity,
}

impl Invocation {
    /// The identity must name the request's attempt and the `Idempotency-Key` header.
    pub fn check(&self, attempt_id: &str) -> Result<(), ApiError> {
        let identity = &self.identity;
        if identity.attempt_id.expose() != attempt_id {
            return Err(ApiError::IdentityMismatch(Mismatch::InvocationAttempt));
        }
        if identity.idempotency_key.expose() != self.headers.idempotency_key.expose() {
            return Err(ApiError::IdentityMismatch(Mismatch::IdempotencyKey));
        }
        if [
            identity.invocation_key.expose(),
            self.headers.correlation_id.expose(),
            self.headers.idempotency_key.expose(),
        ]
        .iter()
        .any(|value| value.is_empty())
        {
            return Err(ApiError::Malformed(MalformedReason::Field));
        }
        Ok(())
    }
}

/// The flat `params` object: the `__ow_*` request parameters beside the action parameters.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct LegacyParams {
    #[serde(
        rename = "__ow_method",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub method: Option<Redacted<String>>,
    #[serde(
        rename = "__ow_headers",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub headers: Option<Redacted<Map<String, Value>>>,
    #[serde(rename = "__ow_path", default, skip_serializing_if = "Option::is_none")]
    pub path: Option<Redacted<String>>,
    #[serde(
        rename = "__ow_query",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub query: Option<Redacted<Map<String, Value>>>,
    #[serde(rename = "__ow_body", default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Redacted<String>>,
    #[serde(flatten)]
    pub action: Redacted<Map<String, Value>>,
}

impl LegacyParams {
    /// Rejects an envelope that must not be invoked: no `__ow_path`, a body over
    /// [`MAX_BODY_BYTES`], or a body that is not JSON under a JSON `content-type`.
    pub fn check(&self) -> Result<(), ApiError> {
        if self
            .path
            .as_ref()
            .is_none_or(|path| path.expose().is_empty())
        {
            return Err(ApiError::Rejected(RequestReason::EnvelopeInvalid));
        }
        let Some(body) = &self.body else {
            return Ok(());
        };
        if body.expose().len() > MAX_BODY_BYTES {
            return Err(ApiError::Rejected(RequestReason::BodyTooLarge));
        }
        if self.json_content_type() && serde_json::from_str::<IgnoredAny>(body.expose()).is_err() {
            return Err(ApiError::Rejected(RequestReason::BodyNotJson));
        }
        Ok(())
    }

    fn json_content_type(&self) -> bool {
        self.headers.as_ref().is_some_and(|headers| {
            headers.expose().iter().any(|(name, value)| {
                name.eq_ignore_ascii_case("content-type")
                    && value.as_str().is_some_and(|v| {
                        v.trim_start()
                            .to_ascii_lowercase()
                            .starts_with("application/json")
                    })
            })
        })
    }
}

/// The handler's result: `statusCode` is an HTTP status, and a 304 has no `body`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyResult {
    #[serde(rename = "statusCode")]
    pub status_code: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<Redacted<Map<String, Value>>>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub body: Option<Redacted<Value>>,
}

impl LegacyResult {
    pub fn check(&self) -> Result<(), ApiError> {
        if !(100..=599).contains(&self.status_code)
            || (self.status_code == 304 && self.body.is_some())
        {
            return Err(ApiError::Malformed(MalformedReason::Field));
        }
        Ok(())
    }
}
