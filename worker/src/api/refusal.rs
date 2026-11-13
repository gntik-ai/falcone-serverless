//! Failure codes, their classes, and the refusal envelope.
//!
//! The three `GW_` codes are named and meant exactly as in ctr-functions-api v1 and are only
//! referenced here. The worker-only codes are the other ones: the contract refusals
//! (`runtime_mode_not_supported`, `invalid_request`, `request_too_large`) and the codes the
//! execution service adds (`undeclared_import`, `busy`, `execution_timeout`, `execution_failed`,
//! `execution_uncertain`, `not_found`, `method_not_allowed`). Nothing in this module reads a
//! request field, and every message is fixed text.

use super::types::{Count, Text};
use super::wire::unit_debug;
use super::{Malformed, constraint};
use serde::{Deserialize, Serialize};
use std::fmt;

/// How a caller may react to a failure. Every v1 code is `non_retryable`; later revisions add
/// codes of the other classes without changing these names.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    Retryable,
    NonRetryable,
    Uncertain,
}
unit_debug!(Class: Retryable, NonRetryable, Uncertain);

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Code {
    #[serde(rename = "GW_OCI_NOT_AUTHORIZED")]
    OciNotAuthorized,
    #[serde(rename = "GW_BASE_DISABLED")]
    BaseDisabled,
    #[serde(rename = "GW_BASE_MISMATCH")]
    BaseMismatch,
    #[serde(rename = "runtime_mode_not_supported")]
    RuntimeModeNotSupported,
    #[serde(rename = "invalid_request")]
    InvalidRequest,
    #[serde(rename = "request_too_large")]
    RequestTooLarge,
    #[serde(rename = "undeclared_import")]
    UndeclaredImport,
    #[serde(rename = "busy")]
    Busy,
    #[serde(rename = "execution_timeout")]
    ExecutionTimeout,
    #[serde(rename = "execution_failed")]
    ExecutionFailed,
    #[serde(rename = "execution_uncertain")]
    ExecutionUncertain,
    #[serde(rename = "not_found")]
    NotFound,
    #[serde(rename = "method_not_allowed")]
    MethodNotAllowed,
}
unit_debug!(
    Code: OciNotAuthorized,
    BaseDisabled,
    BaseMismatch,
    RuntimeModeNotSupported,
    InvalidRequest,
    RequestTooLarge,
    UndeclaredImport,
    Busy,
    ExecutionTimeout,
    ExecutionFailed,
    ExecutionUncertain,
    NotFound,
    MethodNotAllowed,
);

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    OciNotAuthorized,
    BaseDisabled,
    BaseMismatch,
    RuntimeModeNotWasm,
    MissingPath,
    NonJsonBody,
    BodyTooLarge,
    MalformedMessage,
    MalformedHttp,
    BudgetAboveWorkerCeiling,
    EngineVersionMismatch,
    DuplicateAttempt,
    HeadersTooLarge,
    RequestBodyTooLarge,
    ImportNotDeclared,
    ImportNotAllowed,
    AtCapacity,
    DeadlineExceeded,
    MemoryBudgetExceeded,
    FuelExhausted,
    HostCallBudgetExceeded,
    BufferLimitExceeded,
    HandlerFailed,
    HandlerTrapped,
    InvalidHandlerOutput,
    InstantiationFailed,
    ArtifactUnavailable,
    ArtifactRejected,
    Cancelled,
    HostCallOutcomeUnknown,
    UnknownPath,
    UnknownExecution,
    MethodNotAllowed,
}
unit_debug!(
    Reason: OciNotAuthorized,
    BaseDisabled,
    BaseMismatch,
    RuntimeModeNotWasm,
    MissingPath,
    NonJsonBody,
    BodyTooLarge,
    MalformedMessage,
    MalformedHttp,
    BudgetAboveWorkerCeiling,
    EngineVersionMismatch,
    DuplicateAttempt,
    HeadersTooLarge,
    RequestBodyTooLarge,
    ImportNotDeclared,
    ImportNotAllowed,
    AtCapacity,
    DeadlineExceeded,
    MemoryBudgetExceeded,
    FuelExhausted,
    HostCallBudgetExceeded,
    BufferLimitExceeded,
    HandlerFailed,
    HandlerTrapped,
    InvalidHandlerOutput,
    InstantiationFailed,
    ArtifactUnavailable,
    ArtifactRejected,
    Cancelled,
    HostCallOutcomeUnknown,
    UnknownPath,
    UnknownExecution,
    MethodNotAllowed,
);

impl Code {
    fn status(self) -> u16 {
        match self {
            Self::OciNotAuthorized => 403,
            Self::BaseDisabled | Self::BaseMismatch | Self::RuntimeModeNotSupported => 409,
            Self::InvalidRequest => 400,
            Self::RequestTooLarge => 413,
            Self::UndeclaredImport => 422,
            Self::Busy => 503,
            Self::ExecutionTimeout => 504,
            Self::ExecutionFailed => 500,
            Self::ExecutionUncertain => 502,
            Self::NotFound => 404,
            Self::MethodNotAllowed => 405,
        }
    }

    fn message(self) -> &'static str {
        match self {
            Self::OciNotAuthorized => "OCI execution is not authorized for this workspace.",
            Self::BaseDisabled => "The selected base is disabled.",
            Self::BaseMismatch => "The base does not match the pinned digest.",
            Self::RuntimeModeNotSupported => {
                "This worker runs only revisions in runtime mode wasm."
            }
            Self::InvalidRequest => "The request is malformed.",
            Self::RequestTooLarge => "The request body exceeds the size budget.",
            Self::UndeclaredImport => {
                "The component imports an interface the revision did not declare."
            }
            Self::Busy => "The worker is at capacity.",
            Self::ExecutionTimeout => "The execution exceeded its time limit.",
            Self::ExecutionFailed => "The execution failed.",
            Self::ExecutionUncertain => "The outcome of an external effect is not known.",
            Self::NotFound => "No such resource.",
            Self::MethodNotAllowed => "The method is not allowed for this path.",
        }
    }

    /// Retries never change the class: a retried attempt of a code stays in its class. Only
    /// `busy` may be retried; only `execution_uncertain` leaves an external effect unknown.
    pub fn class(self) -> Class {
        match self {
            Self::Busy => Class::Retryable,
            Self::ExecutionUncertain => Class::Uncertain,
            _ => Class::NonRetryable,
        }
    }
    fn allows(self, reason: Reason) -> bool {
        matches!(
            (self, reason),
            (Self::OciNotAuthorized, Reason::OciNotAuthorized)
                | (Self::BaseDisabled, Reason::BaseDisabled)
                | (Self::BaseMismatch, Reason::BaseMismatch)
                | (Self::RuntimeModeNotSupported, Reason::RuntimeModeNotWasm)
                | (
                    Self::InvalidRequest,
                    Reason::MissingPath
                        | Reason::NonJsonBody
                        | Reason::MalformedMessage
                        | Reason::MalformedHttp
                        | Reason::BudgetAboveWorkerCeiling
                        | Reason::EngineVersionMismatch
                        | Reason::DuplicateAttempt
                )
                | (
                    Self::RequestTooLarge,
                    Reason::BodyTooLarge | Reason::HeadersTooLarge | Reason::RequestBodyTooLarge
                )
                | (
                    Self::UndeclaredImport,
                    Reason::ImportNotDeclared | Reason::ImportNotAllowed
                )
                | (Self::Busy, Reason::AtCapacity)
                | (Self::ExecutionTimeout, Reason::DeadlineExceeded)
                | (
                    Self::ExecutionFailed,
                    Reason::MemoryBudgetExceeded
                        | Reason::FuelExhausted
                        | Reason::HostCallBudgetExceeded
                        | Reason::BufferLimitExceeded
                        | Reason::HandlerFailed
                        | Reason::HandlerTrapped
                        | Reason::InvalidHandlerOutput
                        | Reason::InstantiationFailed
                        | Reason::ArtifactUnavailable
                        | Reason::ArtifactRejected
                        | Reason::Cancelled
                )
                | (Self::ExecutionUncertain, Reason::HostCallOutcomeUnknown)
                | (
                    Self::NotFound,
                    Reason::UnknownPath | Reason::UnknownExecution
                )
                | (Self::MethodNotAllowed, Reason::MethodNotAllowed)
        )
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Detail {
    reason: Reason,
}

/// The failure envelope: `status`, `code`, `message`, `detail.reason` and `retryable`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Refusal {
    status: Count,
    code: Code,
    message: Text,
    detail: Detail,
    retryable: bool,
}

impl Refusal {
    pub fn new(code: Code, reason: Reason) -> Self {
        debug_assert!(code.allows(reason));
        Self {
            status: Count::from(code.status()),
            code,
            message: Text::fixed(code.message()),
            detail: Detail { reason },
            retryable: code.class() == Class::Retryable,
        }
    }

    /// A parsed envelope must carry the status, reason and retryability of its code.
    pub fn check(&self) -> Result<(), Malformed> {
        if self.status.is(self.code.status())
            && self.code.allows(self.detail.reason)
            && self.retryable == (self.code.class() == Class::Retryable)
        {
            Ok(())
        } else {
            Err(constraint("result.failure"))
        }
    }

    pub fn class(&self) -> Class {
        self.code.class()
    }

    /// The HTTP status of the refusal.
    pub fn http_status(&self) -> u16 {
        self.code.status()
    }

    /// The bare failure envelope, the HTTP body when there is no activation to attribute.
    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }
}

/// Names the code and reason, which are validated codes; the message is not printed.
impl fmt::Debug for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Refusal({:?}, {:?})", self.code, self.detail.reason)
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "refused: {:?} ({:?})", self.code, self.detail.reason)
    }
}
