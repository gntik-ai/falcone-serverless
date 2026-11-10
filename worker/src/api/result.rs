//! Result envelope, failure codes and classes, runtime identity and the Wasm-worker gate.

use super::projection::{LanguageProfile, RuntimeMode};
use super::request::ExecuteAttempt;
use super::types::{ContractError, Digest, Id, parse};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Success,
    Failure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    Retryable,
    NonRetryable,
    Uncertain,
}

/// The v1 failure code set. The first three are defined by `ctr-functions-api` v1
/// (the lowercase `detail.reason` of `GW_<X>`); `RuntimeModeNotWasm` is the only
/// code defined by the worker contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCode {
    OciNotAuthorized,
    BaseDisabled,
    BaseMismatch,
    RuntimeModeNotWasm,
}

impl FailureCode {
    pub const ALL: [Self; 4] = [
        Self::OciNotAuthorized,
        Self::BaseDisabled,
        Self::BaseMismatch,
        Self::RuntimeModeNotWasm,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::OciNotAuthorized => "oci_not_authorized",
            Self::BaseDisabled => "base_disabled",
            Self::BaseMismatch => "base_mismatch",
            Self::RuntimeModeNotWasm => "runtime_mode_not_wasm",
        }
    }

    /// The class of the code. It does not depend on the attempt: a retry of
    /// `oci_not_authorized` or `base_disabled` is non-retryable too.
    pub fn class(self) -> FailureClass {
        match self {
            Self::OciNotAuthorized
            | Self::BaseDisabled
            | Self::BaseMismatch
            | Self::RuntimeModeNotWasm => FailureClass::NonRetryable,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Failure {
    pub code: FailureCode,
    pub class: FailureClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<Id>,
}

/// The activation runtime identity. A `wasm` identity carries every field; for other
/// modes only the revision and mode are required here (OCI-04 adds the rest), and the
/// mode is absent only on a refusal of a mode the worker does not recognise.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeIdentity {
    pub function_revision: Id,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_mode: Option<RuntimeMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_digest: Option<Digest>,
    /// Present exactly when the revision pins a base (see `ExecuteAttempt::check_result`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_digest: Option<Digest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wasmtime_version: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language_profile: Option<LanguageProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface_version: Option<Id>,
}

/// The legacy `{status, result, logs}` envelope plus the failure block and identity.
/// Parse it with `from_value`/`from_slice`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionResult {
    pub activation_id: Id,
    pub attempt_id: Id,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Map<String, Value>>,
    pub logs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<Failure>,
    pub identity: RuntimeIdentity,
}

impl ExecutionResult {
    pub fn from_value(value: &Value) -> Result<Self, ContractError> {
        let result: Self = parse(value)?;
        result.validate()?;
        Ok(result)
    }

    pub fn from_slice(bytes: &[u8]) -> Result<Self, ContractError> {
        let value = serde_json::from_slice(bytes).map_err(|e| ContractError::from_serde(&e))?;
        Self::from_value(&value)
    }

    /// True for the Wasm worker's non-wasm refusal.
    pub fn is_refusal(&self) -> bool {
        matches!(&self.failure, Some(f) if f.code == FailureCode::RuntimeModeNotWasm)
    }

    fn validate(&self) -> Result<(), ContractError> {
        match (&self.status, &self.result, &self.failure) {
            (Status::Success, Some(_), None) => {}
            (Status::Failure, None, Some(failure)) => {
                if failure.class != failure.code.class() {
                    return Err(ContractError::new(
                        "v1: failure class does not match its code",
                    ));
                }
            }
            _ => {
                return Err(ContractError::new(
                    "v1: success carries result only, failure carries failure only",
                ));
            }
        }
        let identity = &self.identity;
        match identity.runtime_mode {
            Some(RuntimeMode::Wasm) => {
                if self.is_refusal() {
                    return Err(ContractError::new(
                        "v1: a refusal never reports runtime_mode wasm",
                    ));
                }
                if identity.artifact_digest.is_none()
                    || identity.wasmtime_version.is_none()
                    || identity.language_profile.is_none()
                    || identity.interface_version.is_none()
                {
                    return Err(ContractError::new("v1: incomplete wasm runtime identity"));
                }
            }
            Some(RuntimeMode::Oci) => {}
            None if self.is_refusal() => {}
            None => return Err(ContractError::new("v1: identity runtime_mode is required")),
        }
        Ok(())
    }
}

/// Facts about the running worker that a refusal reports in its identity.
#[derive(Clone, Copy, Debug)]
pub struct WorkerFacts<'a> {
    pub wasmtime_version: &'a str,
    pub interface_version: &'a str,
}

/// The Wasm worker's refusal of a revision whose runtime mode is not `wasm`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    activation_id: Id,
    attempt_id: Id,
    function_revision: Id,
    /// The mode that was asked for, when it is one the contract knows.
    requested: Option<RuntimeMode>,
}

impl Refusal {
    /// The non-retryable result for the refused attempt. The refusal ran no guest, so
    /// its identity holds only the revision, the requested mode when known and the
    /// worker's own versions.
    pub fn into_result(self, worker: WorkerFacts<'_>) -> Result<ExecutionResult, ContractError> {
        let code = FailureCode::RuntimeModeNotWasm;
        let result = ExecutionResult {
            activation_id: self.activation_id,
            attempt_id: self.attempt_id,
            status: Status::Failure,
            result: None,
            logs: Vec::new(),
            failure: Some(Failure {
                code,
                class: code.class(),
                message: Some(Id::try_from("runtime_mode is not wasm".to_owned())?),
            }),
            identity: RuntimeIdentity {
                function_revision: self.function_revision,
                runtime_mode: self.requested,
                artifact_digest: None,
                base_digest: None,
                wasmtime_version: Some(Id::try_from(worker.wasmtime_version.to_owned())?),
                language_profile: None,
                interface_version: Some(Id::try_from(worker.interface_version.to_owned())?),
            },
        };
        result.validate()?;
        Ok(result)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum AdmitError {
    /// The request is not a well-formed v1 attempt.
    Malformed(ContractError),
    /// The projection's runtime mode is not `wasm`; the worker executes nothing.
    Refused(Refusal),
}

/// The Wasm worker's admission gate. The runtime mode is read first: a mode other than
/// `wasm` (`oci` or an unknown value) is refused whatever else the projection carries,
/// so fields a later additive revision defines for another mode cannot turn a refusal
/// into a parse error. A missing or non-string mode is malformed.
pub fn admit_wasm_attempt(bytes: &[u8]) -> Result<ExecuteAttempt, AdmitError> {
    let malformed = |message: &str| AdmitError::Malformed(ContractError::new(message));
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| AdmitError::Malformed(ContractError::from_serde(&error)))?;
    let id = |pointer: &str| -> Result<Id, AdmitError> {
        match value.pointer(pointer) {
            Some(Value::String(text)) => Id::try_from(text.clone()).map_err(AdmitError::Malformed),
            _ => Err(malformed("missing field in the attempt envelope")),
        }
    };
    let mode = match value.pointer("/projection/runtime_mode") {
        None => return Err(malformed("missing field `runtime_mode`")),
        Some(Value::String(mode)) => mode.as_str(),
        Some(_) => return Err(malformed("v1: runtime_mode must be a string")),
    };
    if mode != "wasm" {
        return Err(AdmitError::Refused(Refusal {
            activation_id: id("/activation_id")?,
            attempt_id: id("/attempt_id")?,
            function_revision: id("/projection/revision")?,
            requested: (mode == "oci").then_some(RuntimeMode::Oci),
        }));
    }
    ExecuteAttempt::from_value(&value).map_err(AdmitError::Malformed)
}
