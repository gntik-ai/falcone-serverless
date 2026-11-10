//! Result envelope, failure codes and classes, runtime identity and the Wasm-worker gate.

use super::projection::{CatalogBaseId, LanguageProfile, RuntimeMode};
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

/// The activation runtime identity. Every result carries the revision, the raw artifact
/// digest, the language profile and the Wasmtime and interface versions, whatever the
/// mode; `base_digest` follows the pinned base. `runtime_mode` is the mode the projection
/// requested: `wasm` or `oci`, or, on a refusal only, the unrecognised value that was
/// refused (any identifier other than `wasm`). Fields OCI-04 adds are additive and optional.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeIdentity {
    pub function_revision: Id,
    pub runtime_mode: Id,
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
        let mode = identity.runtime_mode.as_str();
        if self.is_refusal() {
            if mode == RuntimeMode::Wasm.as_str() {
                return Err(ContractError::new(
                    "v1: a refusal never reports runtime_mode wasm",
                ));
            }
        } else if mode != RuntimeMode::Wasm.as_str() && mode != RuntimeMode::Oci.as_str() {
            return Err(ContractError::new(
                "v1: identity runtime_mode must be wasm or oci",
            ));
        }
        if identity.artifact_digest.is_none()
            || identity.wasmtime_version.is_none()
            || identity.language_profile.is_none()
            || identity.interface_version.is_none()
        {
            return Err(ContractError::new("v1: incomplete runtime identity"));
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

/// The Wasm worker's refusal of a revision whose runtime mode is not `wasm`. Its `Debug`
/// output omits the requested mode, which is tenant-supplied text; only the refusal
/// result reports it.
#[derive(Clone, PartialEq, Eq)]
pub struct Refusal {
    activation_id: Id,
    attempt_id: Id,
    function_revision: Id,
    /// The mode that was asked for, kept as written (`oci` or an unrecognised value).
    requested: Id,
    artifact_digest: Digest,
    language_profile: LanguageProfile,
    /// The digest of the pinned base; `None` exactly when the revision pins none.
    base_digest: Option<Digest>,
}

impl std::fmt::Debug for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Refusal")
            .field("activation_id", &self.activation_id)
            .field("attempt_id", &self.attempt_id)
            .field("function_revision", &self.function_revision)
            .field("requested", &"<omitted>")
            .finish_non_exhaustive()
    }
}

impl Refusal {
    /// The non-retryable result for the refused attempt. The refusal ran no guest, so
    /// its identity describes the pinned revision (artifact digest, language profile and
    /// the base digest when a base is pinned), the requested mode as written, and the
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
                artifact_digest: Some(self.artifact_digest),
                base_digest: self.base_digest,
                wasmtime_version: Some(Id::try_from(worker.wasmtime_version.to_owned())?),
                language_profile: Some(self.language_profile),
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
    Refused(Box<Refusal>),
}

/// The Wasm worker's admission gate. The runtime mode is read first: a mode other than
/// `wasm` (`oci` or an unknown value) is refused whatever else the projection carries,
/// so fields a later additive revision defines for another mode cannot turn a refusal
/// into a parse error. A missing or non-string mode is malformed, and so is a refused
/// projection that lacks the identity facts a refusal must report: the artifact digest,
/// the language profile, and a well-formed base (digest included) when one is pinned.
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
        let artifact_digest = match value.pointer("/projection/artifact_digest") {
            Some(Value::String(text)) => Digest::try_from(text.clone()),
            _ => Err(ContractError::new("missing field `artifact_digest`")),
        }
        .map_err(AdmitError::Malformed)?;
        let language_profile = match value.pointer("/projection/language_profile") {
            Some(profile) => parse::<LanguageProfile>(profile),
            None => Err(ContractError::new("missing field `language_profile`")),
        }
        .map_err(AdmitError::Malformed)?;
        let base_digest = match value.pointer("/projection/base") {
            None => None,
            Some(base) => Some(pinned_base_digest(base).map_err(AdmitError::Malformed)?),
        };
        return Err(AdmitError::Refused(Box::new(Refusal {
            activation_id: id("/activation_id")?,
            attempt_id: id("/attempt_id")?,
            function_revision: id("/projection/revision")?,
            requested: Id::try_from(mode.to_owned()).map_err(AdmitError::Malformed)?,
            artifact_digest,
            language_profile,
            base_digest,
        })));
    }
    ExecuteAttempt::from_value(&value).map_err(AdmitError::Malformed)
}

/// The digest of a pinned base read from the raw projection of a refused attempt. Only
/// the kind, the digest and (for a catalog base) the id are read, so fields another mode
/// adds to the base cannot turn a refusal into a parse error; an unresolved alias, an
/// unknown kind or a missing or malformed digest is still malformed.
fn pinned_base_digest(base: &Value) -> Result<Digest, ContractError> {
    let text = |key: &str| match base.get(key) {
        Some(Value::String(text)) => Ok(text.clone()),
        _ => Err(ContractError::new("v1: base is missing a required field")),
    };
    match base.get("kind").and_then(Value::as_str) {
        Some("catalog") => {
            CatalogBaseId::try_from(text("id")?)?;
            Digest::try_from(text("digest")?)
        }
        Some("byo") => Digest::try_from(text("image_digest")?),
        _ => Err(ContractError::new("v1: base kind is not catalog or byo")),
    }
}
