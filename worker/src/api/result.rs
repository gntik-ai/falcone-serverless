//! The result: the legacy `{status, result, logs}` envelope plus the activation identity.

use super::Attribution;
use super::projection::{LanguageProfile, Projection};
use super::refusal::{Class, Refusal};
use super::request::ExecuteRequest;
use super::types::{
    Count, Digest, Id, InterfaceVersion, Logs, Positive, RuntimeMode, Text, Version,
};
use super::wire::{constant, present, redacted, unit_debug};
use super::{Malformed, constraint};
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
    fn answering(
        activity: &Attribution,
        status: Status,
        result: Outcome,
        class: Option<Class>,
        logs: Vec<String>,
    ) -> Self {
        Self {
            version: Version::Only,
            message: ResultType::Only,
            activation_id: activity.activation_id.clone(),
            attempt_id: activity.attempt_id.clone(),
            attempt_number: activity.attempt_number,
            status,
            result,
            class,
            logs: Logs::bounded(logs),
            identity: Identity::of(&activity.projection),
        }
    }

    /// The success result for the legacy handler result `bytes`, or `None` when the guest
    /// returned something that is not one.
    pub fn success(activity: &Attribution, bytes: &[u8], logs: Vec<String>) -> Option<Self> {
        let handler: HandlerResult = serde_json::from_slice(bytes).ok()?;
        if !handler.status_code.within(100, 599)
            || (handler.status_code.is(304) && handler.body.is_some())
        {
            return None;
        }
        Some(Self::answering(
            activity,
            Status::Success,
            Outcome::Handler(handler),
            None,
            logs,
        ))
    }

    /// The failure result of `refusal`, with its class.
    pub fn failure(activity: &Attribution, refusal: Refusal, logs: Vec<String>) -> Self {
        let class = refusal.class();
        Self::answering(
            activity,
            Status::Failure,
            Outcome::Failure(refusal),
            Some(class),
            logs,
        )
    }

    /// The HTTP status that carries this result.
    pub fn http_status(&self) -> u16 {
        match &self.result {
            Outcome::Failure(refusal) => refusal.status(),
            Outcome::Handler(_) => 200,
        }
    }

    pub fn to_vec(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// The rules a schema cannot attach to one field, and the identity of the pinned revision.
    pub(super) fn check(&self, pinned: &impl Pinned) -> Result<(), Malformed> {
        match (&self.status, &self.result, &self.class) {
            (Status::Success, Outcome::Handler(handler), None) => {
                if !handler.status_code.within(100, 599) {
                    return Err(constraint("result.statusCode"));
                }
                if handler.status_code.is(304) && handler.body.is_some() {
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
    /// What ran: the revision the projection pins, with the engine it names.
    fn of(projection: &Projection) -> Self {
        Self {
            function_revision: projection.revision.clone(),
            runtime_mode: projection.runtime_mode,
            artifact_digest: projection.artifact_digest.clone(),
            base_digest: projection.base_digest().cloned(),
            wasmtime_version: projection.engine_version.clone(),
            language_profile: projection.language_profile.clone(),
            interface_version: projection.interface_version,
        }
    }

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
