//! The execute message and the checks that admit it to the Wasm worker.

use super::grants::{Grant, GrantVersion, all_valid};
use super::projection::Projection;
use super::refusal::{Code, Reason, Refusal};
use super::types::{Deadline, Id, NonEmpty, Positive, RuntimeMode, Seconds, Version};
use super::wire::{constant, present, redacted};
use super::{Attribution, Malformed, Refused, Rejection, constraint};
use serde::de::{Error as _, IgnoredAny};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

constant!(ExecuteType, "execute");

/// The size budget of a request body: `max-host-to-guest-bytes` in `worker/wit/host.wit`.
const MAX_BODY_BYTES: usize = 1_048_576;

/// The ctr-execution-credential v1 claims. The signed encoding belongs to that contract.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credential {
    iss: NonEmpty,
    aud: NonEmpty,
    tenant: NonEmpty,
    workspace: NonEmpty,
    pool_profile: NonEmpty,
    revision: NonEmpty,
    activation: NonEmpty,
    attempt: NonEmpty,
    iat: Seconds,
    exp: Seconds,
    allowed_capabilities: Vec<Grant>,
}
redacted!(Credential);

/// The invocation identity (`falcone:host/invocation-context`); `attempt-id` is `attempt_id`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invocation {
    invocation_key: Id,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    idempotency_key: Option<Id>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    correlation_id: Option<Id>,
}
redacted!(Invocation);

/// The legacy `main(params)` object: `__ow_*` request fields beside the action parameters.
#[derive(Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Params(Map<String, Value>);
redacted!(Params);

impl<'de> Deserialize<'de> for Params {
    fn deserialize<D: Deserializer<'de>>(input: D) -> Result<Self, D::Error> {
        let map = Map::<String, Value>::deserialize(input)?;
        let strings = |key: &str| map.get(key).is_none_or(Value::is_string);
        let objects = |key: &str| map.get(key).is_none_or(Value::is_object);
        let headers = map.get("__ow_headers").is_none_or(|value| {
            value
                .as_object()
                .is_some_and(|headers| headers.values().all(Value::is_string))
        });
        if strings("__ow_method")
            && strings("__ow_path")
            && strings("__ow_body")
            && objects("__ow_query")
            && headers
        {
            Ok(Self(map))
        } else {
            Err(D::Error::custom("invalid value"))
        }
    }
}

impl Params {
    /// The legacy inputs rejected before invocation: nothing runs for any of them.
    fn check(&self) -> Result<(), Refusal> {
        let invalid = |reason| Refusal::new(Code::InvalidRequest, reason);
        if !self.0.contains_key("__ow_path") {
            return Err(invalid(Reason::MissingPath));
        }
        let Some(body) = self.0.get("__ow_body").and_then(Value::as_str) else {
            return Ok(());
        };
        if body.len() > MAX_BODY_BYTES {
            return Err(Refusal::new(Code::RequestTooLarge, Reason::BodyTooLarge));
        }
        if self.json_content_type() && serde_json::from_str::<IgnoredAny>(body).is_err() {
            return Err(invalid(Reason::NonJsonBody));
        }
        Ok(())
    }

    fn json_content_type(&self) -> bool {
        self.0
            .get("__ow_headers")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter(|(name, _)| name.eq_ignore_ascii_case("content-type"))
            .filter_map(|(_, value)| value.as_str())
            .any(|value| {
                value
                    .split(';')
                    .next()
                    .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"))
            })
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteRequest {
    version: Version,
    #[serde(rename = "type")]
    message: ExecuteType,
    activation_id: Id,
    attempt_id: Id,
    attempt_number: Positive,
    deadline: Deadline,
    invocation: Invocation,
    credential: Credential,
    pub(super) projection: Projection,
    params: Params,
}
redacted!(ExecuteRequest);

impl ExecuteRequest {
    pub(super) fn activation_id(&self) -> &Id {
        &self.activation_id
    }

    pub(super) fn attempt_id(&self) -> &Id {
        &self.attempt_id
    }

    pub(super) fn attempt_number(&self) -> Positive {
        self.attempt_number
    }

    fn attribution(&self) -> Attribution {
        Attribution {
            activation_id: self.activation_id.clone(),
            attempt_id: self.attempt_id.clone(),
            attempt_number: self.attempt_number,
            projection: Box::new(self.projection.clone()),
        }
    }

    /// Admission on the Wasm worker, in this order: a runtime mode other than `wasm` is
    /// refused; the grants must match their schemas' versions; the credential is bound to the
    /// request; the legacy inputs are checked. Nothing is invoked on any error.
    pub(super) fn admit(self) -> Result<Self, Rejection> {
        let refuse = |request: &Self, refusal| {
            Rejection::Refused(Refused::new(refusal, Some(request.attribution())))
        };
        if self.projection.runtime_mode != RuntimeMode::Wasm {
            let refusal = Refusal::new(Code::RuntimeModeNotSupported, Reason::RuntimeModeNotWasm);
            return Err(refuse(&self, refusal));
        }
        self.projection.check()?;
        if !all_valid(&self.credential.allowed_capabilities, GrantVersion::V010) {
            return Err(constraint("credential.allowed_capabilities").into());
        }
        if !(self.activation_id.same_as(&self.credential.activation)
            && self.attempt_id.same_as(&self.credential.attempt)
            && self.projection.revision.same_as(&self.credential.revision))
        {
            return Err(constraint("credential.binding").into());
        }
        self.params
            .check()
            .map_err(|refusal| refuse(&self, refusal))?;
        Ok(self)
    }

    pub(crate) fn key(&self) -> (String, String) {
        (
            self.activation_id.as_str().to_owned(),
            self.attempt_id.as_str().to_owned(),
        )
    }
    /// Only params and the explicitly guest-visible invocation identity cross the host boundary.
    /// Credential claims are deliberately not part of this adapter.
    pub(crate) fn params_bytes(&self) -> Result<Vec<u8>, Malformed> {
        serde_json::to_vec(&self.params).map_err(Into::into)
    }
    pub(crate) fn invocation_identity(&self) -> (String, String, String) {
        (
            self.invocation.invocation_key.as_str().to_owned(),
            self.attempt_id.as_str().to_owned(),
            self.invocation
                .idempotency_key
                .as_ref()
                .map(|key| key.as_str().to_owned())
                .unwrap_or_default(),
        )
    }

    pub(super) fn redact_logs(&self, logs: Vec<String>) -> Vec<String> {
        fn strings(value: &Value, found: &mut Vec<String>, minimum: usize) {
            match value {
                Value::String(value) if value.len() >= minimum => {
                    found.push(value.clone());
                    // Bindings format structured logs as JSON. Scrub the encoded form
                    // too, so quotes, backslashes and control characters cannot bypass it.
                    if let Ok(encoded) = serde_json::to_string(value) {
                        found.push(encoded[1..encoded.len() - 1].to_owned());
                    }
                }
                Value::Array(values) => values
                    .iter()
                    .for_each(|value| strings(value, found, minimum)),
                Value::Object(values) => values
                    .values()
                    .for_each(|value| strings(value, found, minimum)),
                _ => {}
            }
        }
        let Ok(claims) = serde_json::to_value(&self.credential) else {
            return vec!["<redacted>".into()];
        };
        let mut secrets = Vec::new();
        // Short identity/grant strings are ordinary vocabulary, not credentials.
        // Always scrub issuer/audience, including short values, so even a complete
        // credential made solely of short claims cannot survive in a log line.
        strings(&claims, &mut secrets, 8);
        strings(&claims["iss"], &mut secrets, 1);
        strings(&claims["aud"], &mut secrets, 1);
        secrets.sort();
        secrets.dedup();
        secrets.sort_by_key(|value| std::cmp::Reverse(value.len()));
        logs.into_iter()
            .take(1000)
            .map(|mut line| {
                for secret in &secrets {
                    line = line.replace(secret, "<redacted>");
                }
                line.chars().take(8192).collect()
            })
            .collect()
    }
}
