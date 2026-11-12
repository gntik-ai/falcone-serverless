//! Failure codes, their classes, and the refusal envelope.
//!
//! The three `GW_` codes are named and meant exactly as in ctr-functions-api v1 and are only
//! referenced here. The worker-only codes are `runtime_mode_not_supported`, `invalid_request`,
//! `request_too_large`, and the codes the worker service adds: `undeclared_import`, `busy` and
//! `execution_failed`. Nothing in this module reads a request field.

use super::types::{Count, Text};
use super::wire::unit_debug;
use super::{Malformed, constraint};
use serde::{Deserialize, Serialize};
use std::fmt;

/// How a caller may react to a failure. Every code is `non_retryable` except `busy`; a retried
/// attempt of a code never changes its class.
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
    UndeclaredImport,
    Busy,
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
    UndeclaredImport,
    Busy,
    HandlerFailed,
    HandlerTrapped,
    InvalidInput,
    InvalidHandlerResult,
    DeadlineExceeded,
    Cancelled,
    GuestMemoryLimitExceeded,
    BufferLimitExceeded,
    ArtifactUnavailable,
    ArtifactRejected,
    ArtifactDigestMismatch,
    EngineMismatch,
    Uncertain,
    WorkerError,
}
unit_debug!(
    Reason: OciNotAuthorized,
    BaseDisabled,
    BaseMismatch,
    RuntimeModeNotWasm,
    MissingPath,
    NonJsonBody,
    BodyTooLarge,
    UndeclaredImport,
    Busy,
    HandlerFailed,
    HandlerTrapped,
    InvalidInput,
    InvalidHandlerResult,
    DeadlineExceeded,
    Cancelled,
    GuestMemoryLimitExceeded,
    BufferLimitExceeded,
    ArtifactUnavailable,
    ArtifactRejected,
    ArtifactDigestMismatch,
    EngineMismatch,
    Uncertain,
    WorkerError,
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
            Self::UndeclaredImport => {
                "The component imports an interface the revision does not declare."
            }
            Self::Busy => "The worker is at capacity.",
            Self::ExecutionFailed => "The execution failed.",
        }
    }

    /// Retries never change the class: a retried attempt of a non-retryable code stays so.
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
                    Reason::MissingPath | Reason::NonJsonBody
                )
                | (Self::RequestTooLarge, Reason::BodyTooLarge)
                | (Self::UndeclaredImport, Reason::UndeclaredImport)
                | (Self::Busy, Reason::Busy)
                | (
                    Self::ExecutionFailed,
                    Reason::HandlerFailed
                        | Reason::HandlerTrapped
                        | Reason::InvalidInput
                        | Reason::InvalidHandlerResult
                        | Reason::DeadlineExceeded
                        | Reason::Cancelled
                        | Reason::GuestMemoryLimitExceeded
                        | Reason::BufferLimitExceeded
                        | Reason::ArtifactUnavailable
                        | Reason::ArtifactRejected
                        | Reason::ArtifactDigestMismatch
                        | Reason::EngineMismatch
                        | Reason::Uncertain
                        | Reason::WorkerError
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

    /// The HTTP status the contract gives the code.
    pub fn http_status(&self) -> u16 {
        self.code.status()
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
