//! Failure codes, their classes, and the refusal envelope.
//!
//! The three `GW_` codes are named and meant exactly as in ctr-functions-api v1 and are only
//! referenced here. The worker-only codes are `runtime_mode_not_supported`, `invalid_request`
//! and `request_too_large`; service outcomes add bounded admission and execution
//! failures. Nothing in this module reads a request field.

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
    #[serde(rename = "timeout")]
    Timeout,
    #[serde(rename = "busy")]
    Busy,
    #[serde(rename = "undeclared_import")]
    UndeclaredImport,
    #[serde(rename = "cancelled")]
    Cancelled,
    #[serde(rename = "execution_failed")]
    ExecutionFailed,
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
    RequestTooLarge, Timeout, Busy, UndeclaredImport, Cancelled, ExecutionFailed, NotFound, StaleUpdate,
);

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    OciNotAuthorized,
    BaseDisabled,
    BaseMismatch,
    RuntimeModeNotWasm,
    MalformedRequest,
    NotFound,
    StaleUpdate,
    MissingPath,
    NonJsonBody,
    BodyTooLarge,
    ExecutionTimeout,
    CapacityExhausted,
    UndeclaredImport,
    Cancelled,
    ExecutionFailed,
}
unit_debug!(
    Reason: MalformedRequest, NotFound, StaleUpdate, OciNotAuthorized,
    BaseDisabled,
    BaseMismatch,
    RuntimeModeNotWasm,
    MissingPath,
    NonJsonBody,
    BodyTooLarge, ExecutionTimeout, CapacityExhausted, UndeclaredImport, Cancelled, ExecutionFailed,
);

impl Code {
    fn status(self) -> u16 {
        match self {
            Self::OciNotAuthorized => 403,
            Self::BaseDisabled | Self::BaseMismatch | Self::RuntimeModeNotSupported => 409,
            Self::InvalidRequest => 400,
            Self::NotFound => 404,
            Self::StaleUpdate => 409,
            Self::RequestTooLarge => 413,
            Self::Timeout => 504,
            Self::Busy => 503,
            Self::UndeclaredImport => 422,
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
            Self::NotFound => "The operation or attempt was not found.",
            Self::StaleUpdate => "The update sequence is stale.",
            Self::RequestTooLarge => "The request body exceeds the size budget.",
            Self::Timeout => "The execution deadline expired.",
            Self::Busy => "Worker capacity is occupied.",
            Self::UndeclaredImport => "The component imports are not declared and allowed.",
            Self::Cancelled => "The execution was cancelled.",
            Self::ExecutionFailed => "The execution failed.",
        }
    }

    /// Retries never change the class: a retried attempt of a non-retryable code stays so.
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
                    Reason::MissingPath | Reason::NonJsonBody | Reason::MalformedRequest
                )
                | (Self::RequestTooLarge, Reason::BodyTooLarge)
                | (Self::Timeout, Reason::ExecutionTimeout)
                | (Self::Busy, Reason::CapacityExhausted)
                | (Self::UndeclaredImport, Reason::UndeclaredImport)
                | (Self::Cancelled, Reason::Cancelled)
                | (Self::ExecutionFailed, Reason::ExecutionFailed)
                | (Self::NotFound, Reason::NotFound)
                | (Self::StaleUpdate, Reason::StaleUpdate)
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
    pub fn runtime_mode_not_supported() -> Self {
        Self::new(Code::RuntimeModeNotSupported, Reason::RuntimeModeNotWasm)
    }
    pub fn invalid_request() -> Self {
        Self::new(Code::InvalidRequest, Reason::MalformedRequest)
    }
    pub fn not_found() -> Self {
        Self::new(Code::NotFound, Reason::NotFound)
    }
    pub fn stale_update() -> Self {
        Self::new(Code::StaleUpdate, Reason::StaleUpdate)
    }
    pub fn payload_too_large() -> Self {
        Self::new(Code::RequestTooLarge, Reason::BodyTooLarge)
    }
    pub fn timeout() -> Self {
        Self::new(Code::Timeout, Reason::ExecutionTimeout)
    }
    pub fn busy() -> Self {
        Self::new(Code::Busy, Reason::CapacityExhausted)
    }
    pub fn undeclared_import() -> Self {
        Self::new(Code::UndeclaredImport, Reason::UndeclaredImport)
    }
    pub fn cancelled() -> Self {
        Self::new(Code::Cancelled, Reason::Cancelled)
    }
    pub fn execution_failed() -> Self {
        Self::new(Code::ExecutionFailed, Reason::ExecutionFailed)
    }
    pub fn status(&self) -> u16 {
        self.code.status()
    }

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
