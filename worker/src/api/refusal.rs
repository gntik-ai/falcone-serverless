//! Failure codes, their classes, and the refusal envelope.
//!
//! The three `GW_` codes are named and meant exactly as in ctr-functions-api v1 and are only
//! referenced here. The worker-only codes are the refusals `runtime_mode_not_supported`,
//! `invalid_request`, `request_too_large`, `busy`, `undeclared_import`, `not_found` and
//! `stale_update`, and the outcomes `execution_failed`, `timeout` and `cancelled`. Nothing in
//! this module reads a request field.

use super::types::{Count, Text};
use super::wire::unit_debug;
use super::{Malformed, constraint};
use serde::{Deserialize, Serialize};
use std::fmt;

/// How a caller may react to a failure. Only `busy` is `retryable`, because nothing ran; every
/// other v1 code is `non_retryable`.
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
    #[serde(rename = "execution_failed")]
    ExecutionFailed,
    #[serde(rename = "timeout")]
    Timeout,
    #[serde(rename = "cancelled")]
    Cancelled,
    #[serde(rename = "busy")]
    Busy,
    #[serde(rename = "undeclared_import")]
    UndeclaredImport,
    #[serde(rename = "not_found")]
    NotFound,
    #[serde(rename = "stale_update")]
    StaleUpdate,
}
unit_debug!(
    Code: OciNotAuthorized,
    BaseDisabled,
    BaseMismatch,
    RuntimeModeNotSupported,
    InvalidRequest,
    RequestTooLarge,
    ExecutionFailed,
    Timeout,
    Cancelled,
    Busy,
    UndeclaredImport,
    NotFound,
    StaleUpdate,
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
    DuplicateExecution,
    HandlerFailed,
    InvalidHandlerResult,
    GuestTrapped,
    MemoryBudgetExceeded,
    FuelExhausted,
    HostCallBudgetExceeded,
    BufferLimitExceeded,
    ArtifactRejected,
    ArtifactUnavailable,
    SetupFailed,
    OutcomeUncertain,
    DeadlineExceeded,
    CancelRequested,
    AtCapacity,
    ImportNotDeclared,
    ExecutionNotFound,
    SequenceNotNewer,
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
    DuplicateExecution,
    HandlerFailed,
    InvalidHandlerResult,
    GuestTrapped,
    MemoryBudgetExceeded,
    FuelExhausted,
    HostCallBudgetExceeded,
    BufferLimitExceeded,
    ArtifactRejected,
    ArtifactUnavailable,
    SetupFailed,
    OutcomeUncertain,
    DeadlineExceeded,
    CancelRequested,
    AtCapacity,
    ImportNotDeclared,
    ExecutionNotFound,
    SequenceNotNewer,
);

impl Code {
    fn status(self) -> u16 {
        match self {
            Self::OciNotAuthorized => 403,
            Self::BaseDisabled | Self::BaseMismatch | Self::RuntimeModeNotSupported => 409,
            Self::StaleUpdate => 409,
            Self::InvalidRequest => 400,
            Self::RequestTooLarge => 413,
            Self::ExecutionFailed => 500,
            Self::Timeout => 504,
            Self::Cancelled => 499,
            Self::Busy => 503,
            Self::UndeclaredImport => 422,
            Self::NotFound => 404,
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
            Self::ExecutionFailed => "The execution failed.",
            Self::Timeout => "The execution exceeded its time limit.",
            Self::Cancelled => "The execution was cancelled.",
            Self::Busy => "The worker has no free execution capacity.",
            Self::UndeclaredImport => {
                "The component imports an interface the revision does not declare."
            }
            Self::NotFound => "Nothing matches the request on this worker.",
            Self::StaleUpdate => "The update is not newer than the last one accepted.",
        }
    }

    /// Retries never change the class: a retried attempt of a code keeps it. Only `busy` is
    /// retryable, because nothing ran.
    pub(crate) fn class(self) -> Class {
        match self {
            Self::Busy => Class::Retryable,
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
                        | Reason::MalformedRequest
                        | Reason::BudgetAboveWorkerCeiling
                        | Reason::EngineVersionMismatch
                        | Reason::DuplicateExecution
                )
                | (Self::RequestTooLarge, Reason::BodyTooLarge)
                | (
                    Self::ExecutionFailed,
                    Reason::HandlerFailed
                        | Reason::InvalidHandlerResult
                        | Reason::GuestTrapped
                        | Reason::MemoryBudgetExceeded
                        | Reason::FuelExhausted
                        | Reason::HostCallBudgetExceeded
                        | Reason::BufferLimitExceeded
                        | Reason::ArtifactRejected
                        | Reason::ArtifactUnavailable
                        | Reason::SetupFailed
                        | Reason::OutcomeUncertain
                )
                | (Self::Timeout, Reason::DeadlineExceeded)
                | (Self::Cancelled, Reason::CancelRequested)
                | (Self::Busy, Reason::AtCapacity)
                | (Self::UndeclaredImport, Reason::ImportNotDeclared)
                | (Self::NotFound, Reason::ExecutionNotFound)
                | (Self::StaleUpdate, Reason::SequenceNotNewer)
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

    /// The HTTP status the failure carries.
    pub fn status(&self) -> u16 {
        self.code.status()
    }

    pub fn code(&self) -> Code {
        self.code
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
