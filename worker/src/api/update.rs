//! Execution updates (heartbeat, progress, checkpoint reference) and cancel.

use super::{
    error::{ApiError, MalformedReason, Mismatch},
    parse_json,
    redact::Redacted,
    request::ExecuteAttempt,
};
use serde::{Deserialize, Serialize};

/// Sent by the worker while an attempt runs. `sequence` increases within an attempt.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Update {
    Heartbeat {
        activation_id: Redacted<String>,
        attempt_id: Redacted<String>,
        sequence: u64,
    },
    Progress {
        activation_id: Redacted<String>,
        attempt_id: Redacted<String>,
        sequence: u64,
        /// 0..=1000.
        permille: u16,
    },
    /// A reference to a committed checkpoint object (docs/contracts/checkpoint-object.md).
    Checkpoint {
        activation_id: Redacted<String>,
        attempt_id: Redacted<String>,
        sequence: u64,
        checkpoint_ref: Redacted<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelState {
    /// The dispatcher asked for the attempt to stop.
    Requested,
    /// The worker confirms the guest is stopped.
    Acknowledged,
    /// The worker cannot confirm the guest stopped; side effects may exist.
    Uncertain,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Cancel {
    pub activation_id: Redacted<String>,
    pub attempt_id: Redacted<String>,
    pub state: CancelState,
}

fn bound(
    attempt: &ExecuteAttempt,
    activation_id: &Redacted<String>,
    attempt_id: &Redacted<String>,
) -> Result<(), ApiError> {
    if *activation_id != attempt.activation_id {
        return Err(ApiError::IdentityMismatch(Mismatch::ActivationId));
    }
    if *attempt_id != attempt.attempt_id {
        return Err(ApiError::IdentityMismatch(Mismatch::AttemptId));
    }
    Ok(())
}

pub fn parse_update(attempt: &ExecuteAttempt, input: &[u8]) -> Result<Update, ApiError> {
    let update: Update = parse_json(input)?;
    match &update {
        Update::Heartbeat {
            activation_id,
            attempt_id,
            ..
        }
        | Update::Checkpoint {
            activation_id,
            attempt_id,
            ..
        } => bound(attempt, activation_id, attempt_id)?,
        Update::Progress {
            activation_id,
            attempt_id,
            permille,
            ..
        } => {
            bound(attempt, activation_id, attempt_id)?;
            if *permille > 1000 {
                return Err(ApiError::Malformed(MalformedReason::Field));
            }
        }
    }
    Ok(update)
}

pub fn parse_cancel(attempt: &ExecuteAttempt, input: &[u8]) -> Result<Cancel, ApiError> {
    let cancel: Cancel = parse_json(input)?;
    bound(attempt, &cancel.activation_id, &cancel.attempt_id)?;
    Ok(cancel)
}
