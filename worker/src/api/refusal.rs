//! Failure codes, their classes, and the refusal envelope.
//!
//! The three `GW_` codes are named and meant exactly as in ctr-functions-api v1 and are only
//! referenced here. The worker-only codes are the rest of `Code`; the table in
//! `failure-codes.v1.json` lists each with its status, class and reasons. Nothing in this
//! module reads a request field.

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
    MalformedHttp,
    MalformedMessage,
    BudgetAboveWorkerCeiling,
    EngineVersionMismatch,
    HeadersTooLarge,
    HandlerFailed,
    InvalidInput,
    InvalidHandlerResult,
    GuestTrapped,
    InstantiationFailed,
    ArtifactRejected,
    ArtifactUnavailable,
    MemoryBudgetExceeded,
    FuelExhausted,
    HostCallBudgetExceeded,
    BufferLimitExceeded,
    UncertainSideEffects,
    InternalError,
    DeadlineExceeded,
    CancelRequested,
    AtCapacity,
    ImportNotDeclared,
    UnknownOperation,
    UnknownExecution,
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
    MalformedHttp,
    MalformedMessage,
    BudgetAboveWorkerCeiling,
    EngineVersionMismatch,
    HeadersTooLarge,
    HandlerFailed,
    InvalidInput,
    InvalidHandlerResult,
    GuestTrapped,
    InstantiationFailed,
    ArtifactRejected,
    ArtifactUnavailable,
    MemoryBudgetExceeded,
    FuelExhausted,
    HostCallBudgetExceeded,
    BufferLimitExceeded,
    UncertainSideEffects,
    InternalError,
    DeadlineExceeded,
    CancelRequested,
    AtCapacity,
    ImportNotDeclared,
    UnknownOperation,
    UnknownExecution,
    SequenceNotNewer,
);

impl Code {
    /// Every code, in the order of `failure-codes.v1.json`.
    pub const ALL: [Self; 13] = [
        Self::OciNotAuthorized,
        Self::BaseDisabled,
        Self::BaseMismatch,
        Self::RuntimeModeNotSupported,
        Self::InvalidRequest,
        Self::RequestTooLarge,
        Self::ExecutionFailed,
        Self::Timeout,
        Self::Cancelled,
        Self::Busy,
        Self::UndeclaredImport,
        Self::NotFound,
        Self::StaleUpdate,
    ];

    pub fn status(self) -> u16 {
        match self {
            Self::OciNotAuthorized => 403,
            Self::BaseDisabled | Self::BaseMismatch | Self::RuntimeModeNotSupported => 409,
            Self::InvalidRequest => 400,
            Self::RequestTooLarge => 413,
            Self::ExecutionFailed => 500,
            Self::Timeout => 504,
            Self::Cancelled => 499,
            Self::Busy => 503,
            Self::UndeclaredImport => 422,
            Self::NotFound => 404,
            Self::StaleUpdate => 409,
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
            Self::Timeout => "The execution deadline passed.",
            Self::Cancelled => "The execution was cancelled.",
            Self::Busy => "The worker has no free execution capacity.",
            Self::UndeclaredImport => {
                "The component imports an interface the revision does not declare."
            }
            Self::NotFound => "No such operation or execution.",
            Self::StaleUpdate => "The update is older than one already applied.",
        }
    }

    /// The class of a failure with this code and reason. Retries never change it: a retried
    /// attempt of the same activation gets the same status, reason and class.
    pub(crate) fn class(self, reason: Reason) -> Class {
        match (self, reason) {
            (Self::Busy, _) => Class::Retryable,
            // A mutating host call may have committed when the execution ended.
            (Self::Timeout | Self::Cancelled, _)
            | (Self::ExecutionFailed, Reason::UncertainSideEffects) => Class::Uncertain,
            _ => Class::NonRetryable,
        }
    }

    pub(crate) fn allows(self, reason: Reason) -> bool {
        self.reasons().contains(&reason)
    }

    /// Every reason of the code, as listed in `failure-codes.v1.json`.
    pub fn reasons(self) -> &'static [Reason] {
        use Reason::*;
        match self {
            Self::OciNotAuthorized => &[OciNotAuthorized],
            Self::BaseDisabled => &[BaseDisabled],
            Self::BaseMismatch => &[BaseMismatch],
            Self::RuntimeModeNotSupported => &[RuntimeModeNotWasm],
            Self::InvalidRequest => &[
                MissingPath,
                NonJsonBody,
                MalformedHttp,
                MalformedMessage,
                BudgetAboveWorkerCeiling,
                EngineVersionMismatch,
            ],
            Self::RequestTooLarge => &[BodyTooLarge, HeadersTooLarge],
            Self::ExecutionFailed => &[
                HandlerFailed,
                InvalidInput,
                InvalidHandlerResult,
                GuestTrapped,
                InstantiationFailed,
                ArtifactRejected,
                ArtifactUnavailable,
                MemoryBudgetExceeded,
                FuelExhausted,
                HostCallBudgetExceeded,
                BufferLimitExceeded,
                UncertainSideEffects,
                InternalError,
            ],
            Self::Timeout => &[DeadlineExceeded],
            Self::Cancelled => &[CancelRequested],
            Self::Busy => &[AtCapacity],
            Self::UndeclaredImport => &[ImportNotDeclared],
            Self::NotFound => &[UnknownOperation, UnknownExecution],
            Self::StaleUpdate => &[SequenceNotNewer],
        }
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
            retryable: code.class(reason) == Class::Retryable,
        }
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

    /// A parsed envelope must carry the status, reason and retryability of its code.
    pub(crate) fn check(&self) -> Result<(), Malformed> {
        if self.status.is(self.code.status())
            && self.code.allows(self.detail.reason)
            && self.retryable == (self.code.class(self.detail.reason) == Class::Retryable)
        {
            Ok(())
        } else {
            Err(constraint("result.failure"))
        }
    }

    pub fn class(&self) -> Class {
        self.code.class(self.detail.reason)
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
