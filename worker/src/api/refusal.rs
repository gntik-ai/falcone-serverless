//! Failure codes, their classes, and the refusal envelope.
//!
//! The three `GW_` codes are named and meant exactly as in ctr-functions-api v1 and are only
//! referenced here. The worker-only codes are `runtime_mode_not_supported`, `invalid_request`,
//! `request_too_large`, and the outcomes of serving an execution: `busy`, `undeclared_import`,
//! `timeout`, `cancelled` and `execution_failed`. Every failure beyond those is a
//! `detail.reason` of one of them, never a new code. Nothing in this module reads a request
//! field.

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
    #[serde(rename = "busy")]
    Busy,
    #[serde(rename = "undeclared_import")]
    UndeclaredImport,
    #[serde(rename = "timeout")]
    Timeout,
    #[serde(rename = "cancelled")]
    Cancelled,
    #[serde(rename = "execution_failed")]
    ExecutionFailed,
}
unit_debug!(
    Code: OciNotAuthorized,
    BaseDisabled,
    BaseMismatch,
    RuntimeModeNotSupported,
    InvalidRequest,
    RequestTooLarge,
    Busy,
    UndeclaredImport,
    Timeout,
    Cancelled,
    ExecutionFailed,
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
    MalformedRequest,
    BudgetAboveWorkerCeiling,
    EngineVersionMismatch,
    ArtifactDigestMismatch,
    NoCapacity,
    UndeclaredImport,
    DeadlineExceeded,
    CancelRequested,
    MemoryBudgetExceeded,
    FuelExhausted,
    HostCallBudgetExceeded,
    BufferLimitExceeded,
    HandlerFailed,
    HandlerTrapped,
    InstantiationFailed,
    InvalidHandlerResult,
    ArtifactRejected,
    ArtifactUnavailable,
    CompileFailed,
    SetupFailed,
    UncertainSideEffects,
}
unit_debug!(
    Reason: OciNotAuthorized,
    BaseDisabled,
    BaseMismatch,
    RuntimeModeNotWasm,
    MissingPath,
    NonJsonBody,
    BodyTooLarge,
    MalformedRequest,
    BudgetAboveWorkerCeiling,
    EngineVersionMismatch,
    ArtifactDigestMismatch,
    NoCapacity,
    UndeclaredImport,
    DeadlineExceeded,
    CancelRequested,
    MemoryBudgetExceeded,
    FuelExhausted,
    HostCallBudgetExceeded,
    BufferLimitExceeded,
    HandlerFailed,
    HandlerTrapped,
    InstantiationFailed,
    InvalidHandlerResult,
    ArtifactRejected,
    ArtifactUnavailable,
    CompileFailed,
    SetupFailed,
    UncertainSideEffects,
);

impl Code {
    fn status(self) -> u16 {
        match self {
            Self::OciNotAuthorized => 403,
            Self::BaseDisabled | Self::BaseMismatch | Self::RuntimeModeNotSupported => 409,
            Self::InvalidRequest => 400,
            Self::RequestTooLarge => 413,
            Self::Busy => 503,
            Self::UndeclaredImport => 422,
            Self::Timeout => 504,
            // No standard status says "the caller withdrew the request"; 499 is the widely
            // understood one.
            Self::Cancelled => 499,
            Self::ExecutionFailed => 500,
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
            Self::Busy => "The worker has no free execution capacity.",
            Self::UndeclaredImport => {
                "The component imports an interface the revision did not declare."
            }
            Self::Timeout => "The execution reached its deadline.",
            Self::Cancelled => "The execution was cancelled.",
            Self::ExecutionFailed => "The execution failed.",
        }
    }

    /// Retries never change the class: a retried attempt of a non-retryable code stays so.
    /// Every v1 code is `non_retryable`; later revisions add codes of the other classes.
    pub(crate) fn class(self) -> Class {
        Class::NonRetryable
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
                        | Reason::MalformedRequest
                        | Reason::BudgetAboveWorkerCeiling
                        | Reason::EngineVersionMismatch
                        | Reason::ArtifactDigestMismatch
                )
                | (Self::RequestTooLarge, Reason::BodyTooLarge)
                | (Self::Busy, Reason::NoCapacity)
                | (Self::UndeclaredImport, Reason::UndeclaredImport)
                | (Self::Timeout, Reason::DeadlineExceeded)
                | (Self::Cancelled, Reason::CancelRequested)
                | (
                    Self::ExecutionFailed,
                    Reason::MemoryBudgetExceeded
                        | Reason::FuelExhausted
                        | Reason::HostCallBudgetExceeded
                        | Reason::BufferLimitExceeded
                        | Reason::HandlerFailed
                        | Reason::HandlerTrapped
                        | Reason::InstantiationFailed
                        | Reason::InvalidHandlerResult
                        | Reason::ArtifactRejected
                        | Reason::ArtifactUnavailable
                        | Reason::CompileFailed
                        | Reason::SetupFailed
                        | Reason::UncertainSideEffects
                )
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
    pub(crate) fn check(&self) -> Result<(), Malformed> {
        if self.status.is(self.code.status())
            && self.code.allows(self.detail.reason)
            && self.retryable == (self.code.class() == Class::Retryable)
        {
            Ok(())
        } else {
            Err(constraint("result.failure"))
        }
    }

    pub(crate) fn class(&self) -> Class {
        self.code.class()
    }

    pub fn status(&self) -> u16 {
        self.code.status()
    }

    pub fn code(&self) -> Code {
        self.code
    }

    pub fn reason(&self) -> Reason {
        self.detail.reason
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
