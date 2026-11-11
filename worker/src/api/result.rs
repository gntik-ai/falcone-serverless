//! The result: the legacy `{status, result, logs}` envelope plus the failure class and the
//! activation runtime identity, and the Wasm worker's admission of one attempt.
use super::{
    error::{ParseError, Violation},
    failure::{Failure, FailureClass, FailureCode},
    request::{ExecuteRequest, LanguageProfile, RevisionProjection, RuntimeMode},
    text::{Digest, HandlerResult, Name, Text, present},
};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Success,
    Error,
}

/// The activation runtime identity every result carries. `base_digest` is present exactly
/// when the revision pins a base; absent otherwise, never `null`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub function_revision: Name,
    pub runtime_mode: RuntimeMode,
    pub artifact_digest: Digest,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub base_digest: Option<Digest>,
    pub wasmtime_version: Name,
    pub language_profile: LanguageProfile,
    pub interface_version: Name,
}

impl Identity {
    /// The identity a worker reports for `projection`.
    fn of(projection: &RevisionProjection, wasmtime_version: &Name) -> Self {
        Self {
            function_revision: projection.revision.clone(),
            runtime_mode: projection.runtime_mode,
            artifact_digest: projection.artifact_digest.clone(),
            base_digest: projection.base.as_ref().map(|base| base.digest().clone()),
            wasmtime_version: wasmtime_version.clone(),
            language_profile: projection.language_profile.clone(),
            interface_version: projection.interface_version.clone(),
        }
    }

    /// The identity must describe the pinned revision, and `base_digest` is present exactly
    /// when a base is pinned and then equals that base's digest.
    fn check_identity(&self, projection: &RevisionProjection) -> Result<(), Violation> {
        match (&projection.base, &self.base_digest) {
            (Some(_), None) => return Err(Violation::BaseDigestMissing),
            (None, Some(_)) => return Err(Violation::BaseDigestUnexpected),
            (Some(base), Some(digest)) if base.digest() != digest => {
                return Err(Violation::IdentityMismatch);
            }
            _ => {}
        }
        if self.function_revision != projection.revision
            || self.runtime_mode != projection.runtime_mode
            || self.artifact_digest != projection.artifact_digest
            || self.language_profile != projection.language_profile
            || self.interface_version != projection.interface_version
        {
            return Err(Violation::IdentityMismatch);
        }
        Ok(())
    }
}

/// One attempt's result. `result` (the handler return value) is present on success and
/// absent on error; `failure` the other way round. Neither is ever `null`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionResult {
    pub activation_id: Name,
    pub attempt_id: Name,
    pub status: Status,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub result: Option<HandlerResult>,
    pub logs: Vec<Text>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub failure: Option<Failure>,
    pub identity: Identity,
}

impl ExecutionResult {
    fn check(&self, request: &ExecuteRequest) -> Result<(), Violation> {
        if self.activation_id != request.activation_id || self.attempt_id != request.attempt_id {
            return Err(Violation::CorrelationMismatch);
        }
        self.identity.check_identity(&request.projection)?;
        match (self.status, &self.result, &self.failure) {
            (Status::Success, Some(_), None) | (Status::Error, None, Some(_)) => {}
            _ => return Err(Violation::StatusShape),
        }
        if let Some(Failure {
            class,
            code: Some(code),
        }) = &self.failure
            && *class != code.class(request.attempt())
        {
            return Err(Violation::ClassMismatch);
        }
        Ok(())
    }
}

/// Read a result for `request`. The identity block is required, `base_digest` is present
/// exactly when the request pins a base, and a coded failure carries its table class.
pub fn parse_result(request: &ExecuteRequest, bytes: &[u8]) -> Result<ExecutionResult, ParseError> {
    let result: ExecutionResult = serde_json::from_slice(bytes)?;
    result.check(request)?;
    Ok(result)
}

/// A Wasm worker accepted the attempt. Nothing has run: admission is only the mode check.
#[derive(Debug)]
pub struct Admitted;

/// The refusal of an attempt. `Debug` and `Display` print request fields only as
/// `<redacted>`; the serialized `result()` keeps its contract fields.
#[derive(Debug)]
pub struct Refusal {
    code: FailureCode,
    class: FailureClass,
    result: Box<ExecutionResult>,
}

impl Refusal {
    pub fn code(&self) -> FailureCode {
        self.code
    }

    pub fn class(&self) -> FailureClass {
        self.class
    }

    /// The result to send back, in the contract shape.
    pub fn result(&self) -> &ExecutionResult {
        &self.result
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "attempt refused: {} ({}), activation_id={} attempt_id={} revision={}",
            self.code,
            self.class,
            self.result.activation_id,
            self.result.attempt_id,
            self.result.identity.function_revision
        )
    }
}

impl std::error::Error for Refusal {}

/// Admit one attempt on the Wasm worker. A revision whose runtime mode is not `wasm` is
/// refused with the non-retryable `runtime_mode_unsupported`, at every attempt number.
pub fn admit_wasm_attempt(
    request: &ExecuteRequest,
    wasmtime_version: &Name,
) -> Result<Admitted, Refusal> {
    if request.projection.runtime_mode == RuntimeMode::Wasm {
        return Ok(Admitted);
    }
    let code = FailureCode::RuntimeModeUnsupported;
    let class = code.class(request.attempt());
    Err(Refusal {
        code,
        class,
        result: Box::new(ExecutionResult {
            activation_id: request.activation_id.clone(),
            attempt_id: request.attempt_id.clone(),
            status: Status::Error,
            result: None,
            logs: Vec::new(),
            failure: Some(Failure {
                class,
                code: Some(code),
            }),
            identity: Identity::of(&request.projection, wasmtime_version),
        }),
    })
}
