//! Failure codes, classes and the error types of the API. No error type holds a
//! request-controlled string, so none can echo one.

use super::{
    result::{ExecutionResult, Identity},
    types::{Name, Redacted, whole},
};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use serde_json::Number;
use std::fmt;

/// How a caller may react. Every v1 code is `NonRetryable`; `Retryable` and `Uncertain`
/// are reserved for codes that later revisions add.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    Retryable,
    NonRetryable,
    Uncertain,
}

/// The v1 failure codes. The `GW_` codes are the ctr-functions-api v1 codes, referenced by
/// name and meaning; the other three are the only worker-only codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub enum FailureCode {
    #[serde(rename = "GW_OCI_NOT_AUTHORIZED")]
    OciNotAuthorized,
    #[serde(rename = "GW_BASE_DISABLED")]
    BaseDisabled,
    #[serde(rename = "GW_BASE_MISMATCH")]
    BaseMismatch,
    #[serde(rename = "runtime_mode_unsupported")]
    RuntimeModeUnsupported,
    #[serde(rename = "invalid_request")]
    InvalidRequest,
    #[serde(rename = "request_too_large")]
    RequestTooLarge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    OciNotAuthorized,
    BaseDisabled,
    BaseMismatch,
    RuntimeModeUnsupported,
    MissingPath,
    NonJsonBody,
    BodyTooLarge,
}

impl FailureCode {
    pub const ALL: [FailureCode; 6] = [
        Self::OciNotAuthorized,
        Self::BaseDisabled,
        Self::BaseMismatch,
        Self::RuntimeModeUnsupported,
        Self::InvalidRequest,
        Self::RequestTooLarge,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::OciNotAuthorized => "GW_OCI_NOT_AUTHORIZED",
            Self::BaseDisabled => "GW_BASE_DISABLED",
            Self::BaseMismatch => "GW_BASE_MISMATCH",
            Self::RuntimeModeUnsupported => "runtime_mode_unsupported",
            Self::InvalidRequest => "invalid_request",
            Self::RequestTooLarge => "request_too_large",
        }
    }

    pub fn status(self) -> u16 {
        match self {
            Self::OciNotAuthorized => 403,
            Self::BaseDisabled | Self::BaseMismatch | Self::RuntimeModeUnsupported => 409,
            Self::InvalidRequest => 400,
            Self::RequestTooLarge => 413,
        }
    }

    /// Non-retryable on every path, including a retry attempt: the same revision and
    /// request fail the same way again.
    pub fn class(self) -> Class {
        Class::NonRetryable
    }

    pub fn reasons(self) -> &'static [Reason] {
        match self {
            Self::OciNotAuthorized => &[Reason::OciNotAuthorized],
            Self::BaseDisabled => &[Reason::BaseDisabled],
            Self::BaseMismatch => &[Reason::BaseMismatch],
            Self::RuntimeModeUnsupported => &[Reason::RuntimeModeUnsupported],
            Self::InvalidRequest => &[Reason::MissingPath, Reason::NonJsonBody],
            Self::RequestTooLarge => &[Reason::BodyTooLarge],
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::OciNotAuthorized => "OCI execution is not authorized for this workspace.",
            Self::BaseDisabled => "The selected base is disabled.",
            Self::BaseMismatch => "The base does not match the pinned digest.",
            Self::RuntimeModeUnsupported => "This worker executes only the wasm runtime mode.",
            Self::InvalidRequest => "The invocation request is malformed.",
            Self::RequestTooLarge => "The invocation body exceeded the size budget.",
        }
    }

    pub fn worker_only(self) -> bool {
        !self.as_str().starts_with("GW_")
    }
}

impl Reason {
    pub fn code(self) -> FailureCode {
        match self {
            Self::OciNotAuthorized => FailureCode::OciNotAuthorized,
            Self::BaseDisabled => FailureCode::BaseDisabled,
            Self::BaseMismatch => FailureCode::BaseMismatch,
            Self::RuntimeModeUnsupported => FailureCode::RuntimeModeUnsupported,
            Self::MissingPath | Self::NonJsonBody => FailureCode::InvalidRequest,
            Self::BodyTooLarge => FailureCode::RequestTooLarge,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Detail {
    pub reason: Reason,
}

/// The failure `status` is a JSON `integer`: `403`, `403.0` and `4.03e2` are the same value.
fn whole_status<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u16, D::Error> {
    let number = Number::deserialize(deserializer)?;
    whole(&number)
        .and_then(|value| u16::try_from(value).ok())
        .ok_or_else(|| D::Error::custom("status"))
}

/// The failure object of a result: `retryable` is `class == Retryable`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorBody {
    #[serde(deserialize_with = "whole_status")]
    pub status: u16,
    pub code: FailureCode,
    pub message: Name,
    pub detail: Detail,
    pub retryable: bool,
    pub class: Class,
}

impl ErrorBody {
    pub fn new(reason: Reason) -> Self {
        let code = reason.code();
        Self {
            status: code.status(),
            code,
            message: Name::trusted(code.message()),
            detail: Detail { reason },
            retryable: code.class() == Class::Retryable,
            class: code.class(),
        }
    }

    /// Status, reason, class and `retryable` must all be the ones the code defines.
    pub(super) fn check(&self) -> Result<(), Malformed> {
        let code = self.code;
        if self.status == code.status()
            && code.reasons().contains(&self.detail.reason)
            && self.class == code.class()
            && self.retryable == (self.class == Class::Retryable)
        {
            Ok(())
        } else {
            Err(Malformed::Rule("error"))
        }
    }
}

/// A message that cannot be used. Only static categories, never request text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Malformed {
    Syntax,
    Schema,
    TooLarge,
    Rule(&'static str),
}

impl fmt::Display for Malformed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax => f.write_str("malformed execution API message: syntax"),
            Self::Schema => f.write_str("malformed execution API message: schema"),
            Self::TooLarge => f.write_str("malformed execution API message: too large"),
            Self::Rule(rule) => write!(f, "malformed execution API message: rule {rule}"),
        }
    }
}

impl std::error::Error for Malformed {}

/// The worker's refusal of an attempt before anything is invoked: a revision whose runtime
/// mode it does not serve, or a legacy input the contract rejects. It keeps what the
/// serialized result needs; formatting prints only the code and the attempt number.
pub struct Refusal {
    activation_id: Redacted<Name>,
    attempt_id: Redacted<Name>,
    attempt_number: u32,
    identity: Redacted<Identity>,
    reason: Reason,
}

impl Refusal {
    pub(super) fn new(
        activation_id: Name,
        attempt_id: Name,
        attempt_number: u32,
        identity: Identity,
        reason: Reason,
    ) -> Self {
        Self {
            activation_id: Redacted::new(activation_id),
            attempt_id: Redacted::new(attempt_id),
            attempt_number,
            identity: Redacted::new(identity),
            reason,
        }
    }

    pub fn code(&self) -> FailureCode {
        self.reason.code()
    }

    pub fn reason(&self) -> Reason {
        self.reason
    }

    /// The non-retryable failure result for this attempt, with every contract field.
    pub fn result(&self) -> ExecutionResult {
        ExecutionResult::failure_with(
            self.activation_id.get().clone(),
            self.attempt_id.get().clone(),
            self.identity.get().clone(),
            self.reason,
        )
    }
}

impl fmt::Debug for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Refusal")
            .field("code", &self.code().as_str())
            .field("reason", &self.reason)
            .field("attempt_number", &self.attempt_number)
            .field("activation_id", &self.activation_id)
            .field("attempt_id", &self.attempt_id)
            .field("identity", &self.identity)
            .finish()
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "execution refused: {} (non-retryable)",
            self.code().as_str()
        )
    }
}

impl std::error::Error for Refusal {}

/// Why `parse_request` did not return a request to execute.
#[derive(Debug)]
pub enum ApiError {
    Malformed(Malformed),
    Refused(Box<Refusal>),
}

impl From<Malformed> for ApiError {
    fn from(error: Malformed) -> Self {
        Self::Malformed(error)
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(error) => error.fmt(f),
            Self::Refused(refusal) => refusal.fmt(f),
        }
    }
}

impl std::error::Error for ApiError {}
