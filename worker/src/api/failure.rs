//! Failure codes and the retryable / non-retryable / uncertain classes.
use super::text::present;
use serde::{Deserialize, Serialize};
use std::fmt;

/// How a caller treats a failed attempt. The same classes are used by `ctr-functions-api`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    /// Nothing observable happened; a new attempt may run.
    Retryable,
    /// A retry cannot succeed; do not retry.
    NonRetryable,
    /// The attempt may have had an external effect; never replay it blindly.
    Uncertain,
}

impl FailureClass {
    pub const ALL: [Self; 3] = [Self::Retryable, Self::NonRetryable, Self::Uncertain];
}

impl fmt::Display for FailureClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Retryable => "retryable",
            Self::NonRetryable => "non_retryable",
            Self::Uncertain => "uncertain",
        })
    }
}

/// The names of `ctr-functions-api` v1 `detail.reason` for OCI and base failures are
/// referenced here, not redefined; `RuntimeModeUnsupported` is the only worker-only code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCode {
    OciNotAuthorized,
    BaseDisabled,
    BaseMismatch,
    /// The Wasm worker received a revision whose runtime mode is not `wasm`.
    RuntimeModeUnsupported,
}

impl FailureCode {
    pub const ALL: [Self; 4] = [
        Self::OciNotAuthorized,
        Self::BaseDisabled,
        Self::BaseMismatch,
        Self::RuntimeModeUnsupported,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::OciNotAuthorized => "oci_not_authorized",
            Self::BaseDisabled => "base_disabled",
            Self::BaseMismatch => "base_mismatch",
            Self::RuntimeModeUnsupported => "runtime_mode_unsupported",
        }
    }

    /// The class of this code. It does not depend on the attempt number: a retry of a
    /// refused or unauthorized attempt is refused the same way, so no code ever becomes
    /// retryable because `attempt_number` is above one.
    pub fn class(self, _attempt_number: u32) -> FailureClass {
        match self {
            Self::OciNotAuthorized
            | Self::BaseDisabled
            | Self::BaseMismatch
            | Self::RuntimeModeUnsupported => FailureClass::NonRetryable,
        }
    }
}

impl fmt::Display for FailureCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The failure member of an error result. `code` is absent for a failure with no named code,
/// where the class alone says how to treat it; it is never `null`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Failure {
    pub class: FailureClass,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub code: Option<FailureCode>,
}
