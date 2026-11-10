//! API errors. Every variant carries only fixed enums, so formatting one cannot echo a
//! request field, and no message includes a serde error (serde echoes input values).

use super::{
    codes::{FailureClass, FailureCode},
    result::Refusal,
};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MalformedReason {
    /// Not JSON.
    Json,
    /// A type, missing or unknown field, unknown tag or malformed digest.
    Schema,
    /// A field holds an empty, zero or inconsistent value.
    Field,
    /// The base is an unresolved alias rather than `id@version` plus a digest.
    BaseAlias,
    /// `contract_version` is not 1.
    Version,
}

impl MalformedReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Schema => "schema",
            Self::Field => "field",
            Self::BaseAlias => "base_alias",
            Self::Version => "version",
        }
    }
}

/// Why a legacy envelope is rejected before invocation. The labels are the `rejection`
/// values of the inventory v1.1 invalid fixtures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestReason {
    EnvelopeInvalid,
    BodyNotJson,
    BodyTooLarge,
}

impl RequestReason {
    pub const fn label(self) -> &'static str {
        match self {
            Self::EnvelopeInvalid => "envelope-invalid",
            Self::BodyNotJson => "body-not-json",
            Self::BodyTooLarge => "body-too-large",
        }
    }
}

/// Which identity field of a request, credential or result disagrees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mismatch {
    ActivationId,
    AttemptId,
    Revision,
    RuntimeMode,
    ArtifactDigest,
    /// The revision pins a base but the identity block has no `base_digest`.
    BaseDigestMissing,
    /// The identity block has a `base_digest` but the revision pins no base.
    BaseDigestUnexpected,
    BaseDigest,
    WasmtimeVersion,
    LanguageProfile,
    InterfaceVersion,
    FailureClass,
    CredentialActivation,
    CredentialAttempt,
    CredentialRevision,
    InvocationAttempt,
    IdempotencyKey,
}

impl Mismatch {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ActivationId => "activation_id",
            Self::AttemptId => "attempt_id",
            Self::Revision => "revision",
            Self::RuntimeMode => "runtime_mode",
            Self::ArtifactDigest => "artifact_digest",
            Self::BaseDigestMissing => "base_digest_missing",
            Self::BaseDigestUnexpected => "base_digest_unexpected",
            Self::BaseDigest => "base_digest",
            Self::WasmtimeVersion => "wasmtime_version",
            Self::LanguageProfile => "language_profile",
            Self::InterfaceVersion => "interface_version",
            Self::FailureClass => "failure_class",
            Self::CredentialActivation => "credential_activation",
            Self::CredentialAttempt => "credential_attempt",
            Self::CredentialRevision => "credential_revision",
            Self::InvocationAttempt => "invocation_attempt",
            Self::IdempotencyKey => "idempotency_key",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApiError {
    /// A non-retryable refusal: the revision's runtime mode is not `wasm`.
    Refused(Refusal),
    Malformed(MalformedReason),
    Rejected(RequestReason),
    IdentityMismatch(Mismatch),
}

impl ApiError {
    /// The wire failure code, for errors that are reported to the dispatcher.
    pub fn code(&self) -> Option<FailureCode> {
        match self {
            Self::Refused(_) => Some(FailureCode::RuntimeModeNotWasm),
            Self::Malformed(_) | Self::Rejected(_) => Some(FailureCode::InvalidRequest),
            Self::IdentityMismatch(_) => None,
        }
    }

    /// The only text a worker logs for an error: fixed `key=value` tokens, no request field.
    pub fn log_line(&self) -> String {
        let detail = match self {
            Self::Refused(_) => "kind=refused".to_owned(),
            Self::Malformed(reason) => format!("kind=malformed reason={}", reason.as_str()),
            Self::Rejected(reason) => format!("kind=rejected reason={}", reason.label()),
            Self::IdentityMismatch(field) => {
                format!("kind=identity_mismatch field={}", field.as_str())
            }
        };
        match self.code() {
            Some(code) => format!(
                "execution_api {detail} code={} class={}",
                code.as_str(),
                FailureClass::as_str(code.class())
            ),
            None => format!("execution_api {detail}"),
        }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(_) => f.write_str("attempt refused: runtime_mode_not_wasm"),
            Self::Malformed(reason) => write!(f, "malformed v1 document: {}", reason.as_str()),
            Self::Rejected(reason) => write!(f, "request rejected: {}", reason.label()),
            Self::IdentityMismatch(field) => write!(f, "identity mismatch: {}", field.as_str()),
        }
    }
}

impl std::error::Error for ApiError {}
