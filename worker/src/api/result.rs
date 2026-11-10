//! The result: the legacy `{status, result, logs}` envelope, a failure for a failed attempt,
//! and the activation runtime identity block every admitted attempt's result carries.

use super::{
    codes::{FailureClass, FailureCode},
    error::{ApiError, MalformedReason, Mismatch},
    legacy::LegacyResult,
    parse_json,
    redact::{Digest, Redacted},
    request::{ExecuteAttempt, RuntimeMode},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// The handler returned; `result` holds its legacy envelope.
    Success,
    /// The handler raised or returned an error; `result` may hold it.
    ApplicationError,
    /// The platform could not complete the attempt; `failure` holds the code and class.
    Failure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Failure {
    pub code: FailureCode,
    pub class: FailureClass,
}

/// What the worker actually ran. `base_digest` is present exactly when the revision pins a
/// base: the catalog digest, or the BYO image digest.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActivationIdentity {
    pub revision: Redacted<String>,
    pub runtime_mode: RuntimeMode,
    pub artifact_digest: Digest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_digest: Option<Digest>,
    pub wasmtime_version: Redacted<String>,
    pub language_profile: Redacted<String>,
    pub interface_version: Redacted<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionResult {
    pub activation_id: Redacted<String>,
    pub attempt_id: Redacted<String>,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<LegacyResult>,
    pub logs: Vec<Redacted<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<Failure>,
    pub identity: ActivationIdentity,
}

impl ExecutionResult {
    /// Compiled in all builds: the result must describe this attempt and this projection.
    pub fn check_identity(&self, attempt: &ExecuteAttempt) -> Result<(), ApiError> {
        let projection = &attempt.projection;
        let identity = &self.identity;
        for (matches, field) in [
            (
                self.activation_id == attempt.activation_id,
                Mismatch::ActivationId,
            ),
            (self.attempt_id == attempt.attempt_id, Mismatch::AttemptId),
            (identity.revision == projection.revision, Mismatch::Revision),
            (
                identity.runtime_mode == projection.runtime_mode,
                Mismatch::RuntimeMode,
            ),
            (
                identity.artifact_digest == projection.artifact_digest,
                Mismatch::ArtifactDigest,
            ),
        ] {
            if !matches {
                return Err(ApiError::IdentityMismatch(field));
            }
        }
        match (projection.base.as_ref(), identity.base_digest.as_ref()) {
            (Some(_), None) => {
                return Err(ApiError::IdentityMismatch(Mismatch::BaseDigestMissing));
            }
            (None, Some(_)) => {
                return Err(ApiError::IdentityMismatch(Mismatch::BaseDigestUnexpected));
            }
            (Some(base), Some(digest)) if base.digest() != digest => {
                return Err(ApiError::IdentityMismatch(Mismatch::BaseDigest));
            }
            _ => {}
        }
        for (matches, field) in [
            (
                identity.wasmtime_version == projection.engine_version,
                Mismatch::WasmtimeVersion,
            ),
            (
                identity.language_profile == projection.language_profile,
                Mismatch::LanguageProfile,
            ),
            (
                identity.interface_version == projection.interface_version,
                Mismatch::InterfaceVersion,
            ),
        ] {
            if !matches {
                return Err(ApiError::IdentityMismatch(field));
            }
        }
        Ok(())
    }

    fn check_shape(&self) -> Result<(), ApiError> {
        let consistent = match (self.status, &self.failure) {
            (Status::Failure, Some(_)) => self.result.is_none(),
            (Status::Success, None) => self.result.is_some(),
            (Status::ApplicationError, None) => true,
            _ => false,
        };
        if !consistent {
            return Err(ApiError::Malformed(MalformedReason::Field));
        }
        if let Some(failure) = &self.failure
            && failure.class != failure.code.class()
        {
            return Err(ApiError::IdentityMismatch(Mismatch::FailureClass));
        }
        self.result.as_ref().map_or(Ok(()), LegacyResult::check)
    }
}

/// The public result entry point. It takes the admitted attempt and enforces the identity
/// block against its projection, so a result that omits `base_digest` for a pinned base, or
/// carries one without a pinned base, is rejected in every build.
pub fn parse_result(attempt: &ExecuteAttempt, input: &[u8]) -> Result<ExecutionResult, ApiError> {
    let result: ExecutionResult = parse_json(input)?;
    result.check_shape()?;
    result.check_identity(attempt)?;
    Ok(result)
}

/// What a non-`wasm` revision is refused with. It is produced before any projection field
/// other than the identifiers is read, so it has no identity block.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Refusal {
    pub activation_id: Redacted<String>,
    pub attempt_id: Redacted<String>,
    pub revision: Redacted<String>,
    pub requested_runtime_mode: RequestedMode,
    pub status: Status,
    pub logs: Vec<Redacted<String>>,
    pub failure: Failure,
}

/// The refused mode, as a fixed value: the request's own text is never echoed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestedMode {
    Oci,
    Unrecognized,
}

impl Refusal {
    pub fn new(
        activation_id: String,
        attempt_id: String,
        revision: String,
        requested: RequestedMode,
    ) -> Self {
        Self {
            activation_id: Redacted::new(activation_id),
            attempt_id: Redacted::new(attempt_id),
            revision: Redacted::new(revision),
            requested_runtime_mode: requested,
            status: Status::Failure,
            logs: Vec::new(),
            failure: Failure {
                code: FailureCode::RuntimeModeNotWasm,
                class: FailureClass::NonRetryable,
            },
        }
    }
}
