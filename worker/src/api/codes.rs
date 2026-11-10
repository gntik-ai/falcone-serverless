//! Failure codes and classes, defined once across ctr-functions-api and this contract.

use serde::{Deserialize, Serialize};

/// How a caller may treat a failed attempt. The class belongs to the code and never changes
/// with the attempt number: a retry of a non-retryable code is still non-retryable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    Retryable,
    NonRetryable,
    Uncertain,
}

impl FailureClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Retryable => "retryable",
            Self::NonRetryable => "non_retryable",
            Self::Uncertain => "uncertain",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCode {
    /// ctr-functions-api v1 `GW_OCI_NOT_AUTHORIZED`.
    OciNotAuthorized,
    /// ctr-functions-api v1 `GW_BASE_DISABLED`.
    BaseDisabled,
    /// ctr-functions-api v1 `GW_BASE_MISMATCH`.
    BaseMismatch,
    /// Worker-only: the Wasm worker refuses any `runtime_mode` other than `wasm`.
    RuntimeModeNotWasm,
}

impl FailureCode {
    pub const ALL: [Self; 4] = [
        Self::OciNotAuthorized,
        Self::BaseDisabled,
        Self::BaseMismatch,
        Self::RuntimeModeNotWasm,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OciNotAuthorized => "oci_not_authorized",
            Self::BaseDisabled => "base_disabled",
            Self::BaseMismatch => "base_mismatch",
            Self::RuntimeModeNotWasm => "runtime_mode_not_wasm",
        }
    }

    pub const fn class(self) -> FailureClass {
        match self {
            Self::OciNotAuthorized
            | Self::BaseDisabled
            | Self::BaseMismatch
            | Self::RuntimeModeNotWasm => FailureClass::NonRetryable,
        }
    }

    /// The ctr-functions-api v1 gateway code this code is referenced from, by name
    /// (`detail.reason` equals [`Self::as_str`]); `None` for a worker-only code.
    pub const fn gateway_code(self) -> Option<&'static str> {
        match self {
            Self::OciNotAuthorized => Some("GW_OCI_NOT_AUTHORIZED"),
            Self::BaseDisabled => Some("GW_BASE_DISABLED"),
            Self::BaseMismatch => Some("GW_BASE_MISMATCH"),
            _ => None,
        }
    }
}
