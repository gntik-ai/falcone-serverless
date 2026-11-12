//! The result: the legacy `{status, result, logs}` envelope plus the activation identity.

use super::projection::{LanguageProfile, Projection};
use super::refusal::{Class, Refusal};
use super::request::ExecuteRequest;
use super::types::{
    Count, Digest, Id, InterfaceVersion, Logs, Positive, RuntimeMode, Text, Version,
};
use super::wire::{constant, present, redacted, unit_debug};
use super::{Attribution, Malformed, constraint};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

constant!(ResultType, "result");

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Success,
    Failure,
}
unit_debug!(Status: Success, Failure);

/// The legacy handler result: `{statusCode, headers?, body?}`; a 304 has no body.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HandlerResult {
    status_code: Count,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    headers: Option<BTreeMap<String, String>>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    body: Option<Value>,
}
redacted!(HandlerResult);

impl HandlerResult {
    /// The legacy result of a handler's output bytes, which must be JSON. An object with a
    /// `statusCode` is the legacy `{statusCode, headers?, body?}` result and must be exactly
    /// that; any other JSON value is the body of a 200 (a `null` leaves the body absent).
    /// `None` when the output is not a valid handler result.
    pub fn from_output(bytes: &[u8]) -> Option<Self> {
        let value: Value = serde_json::from_slice(bytes).ok()?;
        let legacy = value
            .as_object()
            .is_some_and(|object| object.contains_key("statusCode"));
        let handler = if legacy {
            serde_json::from_value::<Self>(value).ok()?
        } else {
            Self {
                status_code: Count::from(200),
                headers: None,
                body: Some(value).filter(|body| !body.is_null()),
            }
        };
        handler.valid().then_some(handler)
    }

    /// A status from 100 to 599; a 304 has no body.
    fn valid(&self) -> bool {
        self.status_code.within(100, 599) && !(self.status_code.is(304) && self.body.is_some())
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Outcome {
    Failure(Refusal),
    Handler(HandlerResult),
}
redacted!(Outcome);

/// What ran: the activation runtime identity every result carries.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    function_revision: Id,
    runtime_mode: RuntimeMode,
    artifact_digest: Digest,
    /// Present exactly when the revision pins a base; `null` is malformed in either case.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    base_digest: Option<Digest>,
    wasmtime_version: Text,
    language_profile: LanguageProfile,
    interface_version: InterfaceVersion,
}
redacted!(Identity);

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionResult {
    version: Version,
    #[serde(rename = "type")]
    message: ResultType,
    activation_id: Id,
    attempt_id: Id,
    attempt_number: Positive,
    status: Status,
    result: Outcome,
    /// Present exactly when `status` is `failure`.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    class: Option<Class>,
    logs: Logs,
    identity: Identity,
}
redacted!(ExecutionResult);

/// What a result is checked against: the pinned projection, and for a request also the
/// activation and attempt it answers.
pub trait Pinned {
    fn projection(&self) -> &Projection;
    fn attempt(&self) -> Option<(&Id, &Id, Positive)>;
}

impl Pinned for Projection {
    fn projection(&self) -> &Projection {
        self
    }

    fn attempt(&self) -> Option<(&Id, &Id, Positive)> {
        None
    }
}

impl Pinned for ExecuteRequest {
    fn projection(&self) -> &Projection {
        &self.projection
    }

    fn attempt(&self) -> Option<(&Id, &Id, Positive)> {
        Some((
            self.activation_id(),
            self.attempt_id(),
            self.attempt_number(),
        ))
    }
}

impl ExecutionResult {
    /// A success result for the activation `attribution` names, with the runtime identity of its
    /// projection. `logs` are bounded here (1000 lines of 8192 characters) as the contract says.
    pub fn success(attribution: &Attribution, handler: HandlerResult, logs: Vec<String>) -> Self {
        Self::answer(attribution, Outcome::Handler(handler), None, logs)
    }

    /// A failure result: the refusal is the failure envelope and its class is the code's.
    pub fn failure(attribution: &Attribution, refusal: Refusal, logs: Vec<String>) -> Self {
        let class = refusal.class();
        Self::answer(attribution, Outcome::Failure(refusal), Some(class), logs)
    }

    fn answer(
        attribution: &Attribution,
        result: Outcome,
        class: Option<Class>,
        logs: Vec<String>,
    ) -> Self {
        let projection = &attribution.projection;
        Self {
            version: Version::Only,
            message: ResultType::Only,
            activation_id: attribution.activation_id.clone(),
            attempt_id: attribution.attempt_id.clone(),
            attempt_number: attribution.attempt_number,
            status: if class.is_some() {
                Status::Failure
            } else {
                Status::Success
            },
            result,
            class,
            logs: Logs::bounded(logs),
            identity: Identity {
                function_revision: projection.revision.clone(),
                runtime_mode: projection.runtime_mode,
                artifact_digest: projection.artifact_digest.clone(),
                base_digest: projection.base_digest().cloned(),
                wasmtime_version: projection.engine_version.clone(),
                language_profile: projection.language_profile.clone(),
                interface_version: projection.interface_version,
            },
        }
    }

    /// The HTTP status of the reply that carries this result: 200 for a success (the handler's
    /// own `statusCode` is inside), the code's status for a failure.
    pub fn http_status(&self) -> u16 {
        match &self.result {
            Outcome::Failure(refusal) => refusal.http_status(),
            Outcome::Handler(_) => 200,
        }
    }

    /// The rules a schema cannot attach to one field, and the identity of the pinned revision.
    pub(super) fn check(&self, pinned: &impl Pinned) -> Result<(), Malformed> {
        match (&self.status, &self.result, &self.class) {
            (Status::Success, Outcome::Handler(handler), None) => {
                if !handler.status_code.within(100, 599) {
                    return Err(constraint("result.statusCode"));
                }
                if !handler.valid() {
                    return Err(constraint("result.body"));
                }
            }
            (Status::Failure, Outcome::Failure(refusal), Some(class)) => {
                refusal.check()?;
                if *class != refusal.class() {
                    return Err(constraint("result.class"));
                }
            }
            _ => return Err(constraint("result.status")),
        }
        if let Some((activation, attempt, number)) = pinned.attempt()
            && !(self.activation_id == *activation
                && self.attempt_id == *attempt
                && self.attempt_number == number)
        {
            return Err(constraint("result.attempt"));
        }
        self.identity.check(pinned.projection())
    }
}

impl Identity {
    /// The identity must describe the pinned revision, and `base_digest` is present exactly
    /// when the projection pins a base (both inverse mutations are rejected).
    fn check(&self, projection: &Projection) -> Result<(), Malformed> {
        let pinned = projection.base_digest();
        if self.base_digest.as_ref() != pinned {
            return Err(constraint("identity.base_digest"));
        }
        if self.function_revision == projection.revision
            && self.runtime_mode == projection.runtime_mode
            && self.artifact_digest == projection.artifact_digest
            && self.wasmtime_version == projection.engine_version
            && self.language_profile == projection.language_profile
            && self.interface_version == projection.interface_version
        {
            Ok(())
        } else {
            Err(constraint("identity.mismatch"))
        }
    }
}
