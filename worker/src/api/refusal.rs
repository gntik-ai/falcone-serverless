//! Failure codes, their classes, and the refusal envelope.
//!
//! The three `GW_` codes are named and meant exactly as in ctr-functions-api v1 and are only
//! referenced here. The worker-only codes are `runtime_mode_not_supported`, `invalid_request`
//! and `request_too_large`, and the codes of an execution that did not succeed (`invalid_input`,
//! `execution_cancelled`, `deadline_exceeded`, `guest_not_runnable`, `execution_failed` and
//! `execution_uncertain`). Nothing in this module reads a request field.

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
    #[serde(rename = "invalid_input")]
    InvalidInput,
    #[serde(rename = "execution_cancelled")]
    ExecutionCancelled,
    #[serde(rename = "deadline_exceeded")]
    DeadlineExceeded,
    #[serde(rename = "guest_not_runnable")]
    GuestNotRunnable,
    #[serde(rename = "execution_failed")]
    ExecutionFailed,
    #[serde(rename = "execution_uncertain")]
    ExecutionUncertain,
}
unit_debug!(
    Code: OciNotAuthorized,
    BaseDisabled,
    BaseMismatch,
    RuntimeModeNotSupported,
    InvalidRequest,
    RequestTooLarge,
    InvalidInput,
    ExecutionCancelled,
    DeadlineExceeded,
    GuestNotRunnable,
    ExecutionFailed,
    ExecutionUncertain,
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
    InvalidInput,
    Cancelled,
    DeadlineExceeded,
    InstantiationFailed,
    MissingHandlerExport,
    InvalidHandlerSignature,
    EpochInterruptionRequired,
    FuelRequired,
    HandlerFailed,
    HandlerTrapped,
    InvalidHandlerResult,
    BufferLimitExceeded,
    GuestMemoryLimitExceeded,
    Unclassified,
    Uncertain,
}
unit_debug!(
    Reason: OciNotAuthorized,
    BaseDisabled,
    BaseMismatch,
    RuntimeModeNotWasm,
    MissingPath,
    NonJsonBody,
    BodyTooLarge,
    InvalidInput,
    Cancelled,
    DeadlineExceeded,
    InstantiationFailed,
    MissingHandlerExport,
    InvalidHandlerSignature,
    EpochInterruptionRequired,
    FuelRequired,
    HandlerFailed,
    HandlerTrapped,
    InvalidHandlerResult,
    BufferLimitExceeded,
    GuestMemoryLimitExceeded,
    Unclassified,
    Uncertain,
);

impl Code {
    fn status(self) -> u16 {
        match self {
            Self::OciNotAuthorized => 403,
            Self::BaseDisabled
            | Self::BaseMismatch
            | Self::RuntimeModeNotSupported
            | Self::ExecutionCancelled => 409,
            Self::InvalidRequest | Self::InvalidInput => 400,
            Self::RequestTooLarge => 413,
            Self::GuestNotRunnable => 422,
            Self::ExecutionFailed | Self::ExecutionUncertain => 502,
            Self::DeadlineExceeded => 504,
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
            Self::InvalidInput => "The handler rejected its input.",
            Self::ExecutionCancelled => "The execution was cancelled.",
            Self::DeadlineExceeded => "The execution exceeded its deadline.",
            Self::GuestNotRunnable => "The component could not be run as a handler.",
            Self::ExecutionFailed => "The handler did not produce a result.",
            Self::ExecutionUncertain => "The outcome of the execution is not known.",
        }
    }

    /// Retries never change the class: a retried attempt of a non-retryable code stays so. Only
    /// an execution whose outcome is unknown is `uncertain`.
    pub(crate) fn class(self) -> Class {
        match self {
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
                    Reason::MissingPath | Reason::NonJsonBody
                )
                | (Self::RequestTooLarge, Reason::BodyTooLarge)
                | (Self::InvalidInput, Reason::InvalidInput)
                | (Self::ExecutionCancelled, Reason::Cancelled)
                | (Self::DeadlineExceeded, Reason::DeadlineExceeded)
                | (
                    Self::GuestNotRunnable,
                    Reason::InstantiationFailed
                        | Reason::MissingHandlerExport
                        | Reason::InvalidHandlerSignature
                        | Reason::EpochInterruptionRequired
                        | Reason::FuelRequired
                )
                | (
                    Self::ExecutionFailed,
                    Reason::HandlerFailed
                        | Reason::HandlerTrapped
                        | Reason::InvalidHandlerResult
                        | Reason::BufferLimitExceeded
                        | Reason::GuestMemoryLimitExceeded
                        | Reason::Unclassified
                )
                | (Self::ExecutionUncertain, Reason::Uncertain)
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
    pub(crate) fn new(code: Code, reason: Reason) -> Self {
        debug_assert!(code.allows(reason));
        Self {
            status: Count::from(code.status()),
            code,
            message: Text::fixed(code.message()),
            detail: Detail { reason },
            retryable: code.class() == Class::Retryable,
        }
    }

    /// The failure of an execution that did not succeed, from the host's outcome code
    /// (`invalid_input`, `cancelled`, `handler_trapped`, ...). A code the contract does not name
    /// is `execution_failed` with reason `unclassified`; the guest's own text is never carried.
    pub fn executed(host_code: &str) -> Self {
        let (code, reason) = match host_code {
            "invalid_input" => (Code::InvalidInput, Reason::InvalidInput),
            "cancelled" => (Code::ExecutionCancelled, Reason::Cancelled),
            "deadline_exceeded" => (Code::DeadlineExceeded, Reason::DeadlineExceeded),
            "instantiation_failed" => (Code::GuestNotRunnable, Reason::InstantiationFailed),
            "missing_handler_export" => (Code::GuestNotRunnable, Reason::MissingHandlerExport),
            "invalid_handler_signature" => {
                (Code::GuestNotRunnable, Reason::InvalidHandlerSignature)
            }
            "epoch_interruption_required" => {
                (Code::GuestNotRunnable, Reason::EpochInterruptionRequired)
            }
            "fuel_required" => (Code::GuestNotRunnable, Reason::FuelRequired),
            "handler_failed" => (Code::ExecutionFailed, Reason::HandlerFailed),
            "handler_trapped" => (Code::ExecutionFailed, Reason::HandlerTrapped),
            "invalid_handler_result" => (Code::ExecutionFailed, Reason::InvalidHandlerResult),
            "buffer_limit_exceeded" => (Code::ExecutionFailed, Reason::BufferLimitExceeded),
            "guest_memory_limit_exceeded" => {
                (Code::ExecutionFailed, Reason::GuestMemoryLimitExceeded)
            }
            "uncertain" => (Code::ExecutionUncertain, Reason::Uncertain),
            _ => (Code::ExecutionFailed, Reason::Unclassified),
        };
        Self::new(code, reason)
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

    /// The HTTP status of the refusal, passed through unchanged.
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
