//! Execution updates (heartbeat, progress, checkpoint reference) and cancel.
use super::{
    error::ParseError,
    text::{Name, present},
};
use serde::{Deserialize, Serialize};

/// A worker-to-dispatcher update for a running attempt. `sequence` increases per attempt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExecutionUpdate {
    Heartbeat {
        activation_id: Name,
        attempt_id: Name,
        sequence: u64,
    },
    Progress {
        activation_id: Name,
        attempt_id: Name,
        sequence: u64,
        /// Whole percent, 0 to 100; absent when the handler reports none.
        #[serde(
            default,
            deserialize_with = "present",
            skip_serializing_if = "Option::is_none"
        )]
        percent: Option<Percent>,
    },
    /// A reference to a committed checkpoint object, never the checkpoint bytes.
    Checkpoint {
        activation_id: Name,
        attempt_id: Name,
        sequence: u64,
        checkpoint_ref: Name,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u8")]
pub struct Percent(u8);

impl TryFrom<u8> for Percent {
    type Error = &'static str;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value <= 100 {
            Ok(Self(value))
        } else {
            Err("invalid percent")
        }
    }
}

/// Ask the worker to cancel one attempt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelRequest {
    pub activation_id: Name,
    pub attempt_id: Name,
}

/// Where a cancel stands. `Uncertain` means the worker cannot say whether the guest stopped
/// or whether external effects happened; side effects are never claimed reversed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelState {
    Requested,
    Acknowledged,
    Uncertain,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelReport {
    pub activation_id: Name,
    pub attempt_id: Name,
    pub state: CancelState,
}

pub fn parse_update(bytes: &[u8]) -> Result<ExecutionUpdate, ParseError> {
    Ok(serde_json::from_slice(bytes)?)
}

pub fn parse_cancel_request(bytes: &[u8]) -> Result<CancelRequest, ParseError> {
    Ok(serde_json::from_slice(bytes)?)
}

pub fn parse_cancel_report(bytes: &[u8]) -> Result<CancelReport, ParseError> {
    Ok(serde_json::from_slice(bytes)?)
}
