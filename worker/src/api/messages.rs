//! Execution updates (heartbeat, progress, checkpoint reference) and cancellation.

use super::types::{Count, Id, Positive, Text, Version};
use super::wire::{constant, present, redacted, unit_debug};
use super::{Malformed, constraint};
use serde::{Deserialize, Serialize};

constant!(UpdateType, "update");
constant!(CancelType, "cancel");

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Update {
    Heartbeat {},
    Progress {
        completed: Count,
        #[serde(
            default,
            deserialize_with = "present",
            skip_serializing_if = "Option::is_none"
        )]
        total: Option<Positive>,
    },
    /// A reference to a committed checkpoint object, never its payload.
    Checkpoint {
        checkpoint_ref: Text,
    },
}
redacted!(Update);

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateMessage {
    version: Version,
    #[serde(rename = "type")]
    message: UpdateType,
    activation_id: Id,
    attempt_id: Id,
    sequence: Positive,
    update: Update,
}
redacted!(UpdateMessage);

impl UpdateMessage {
    pub fn activation_id(&self) -> &Id {
        &self.activation_id
    }

    pub fn attempt_id(&self) -> &Id {
        &self.attempt_id
    }

    pub fn sequence(&self) -> u64 {
        self.sequence.get()
    }

    pub(super) fn check(&self) -> Result<(), Malformed> {
        match &self.update {
            Update::Progress {
                completed,
                total: Some(total),
            } if completed.exceeds(*total) => Err(constraint("update.progress")),
            _ => Ok(()),
        }
    }
}

/// `requested` and `uncertain` carry no claim that external side effects were reversed.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Cancel {
    Requested {},
    Acknowledged {},
    Uncertain {},
}
redacted!(Cancel);

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelMessage {
    version: Version,
    #[serde(rename = "type")]
    message: CancelType,
    activation_id: Id,
    attempt_id: Id,
    cancel: Cancel,
}
redacted!(CancelMessage);

/// The state a cancel message carries.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CancelState {
    Requested,
    Acknowledged,
    Uncertain,
}
unit_debug!(CancelState: Requested, Acknowledged, Uncertain);

impl CancelMessage {
    pub fn activation_id(&self) -> &Id {
        &self.activation_id
    }

    pub fn attempt_id(&self) -> &Id {
        &self.attempt_id
    }

    pub fn state(&self) -> CancelState {
        match self.cancel {
            Cancel::Requested {} => CancelState::Requested,
            Cancel::Acknowledged {} => CancelState::Acknowledged,
            Cancel::Uncertain {} => CancelState::Uncertain,
        }
    }

    /// The worker's answer to a cancel request for the same attempt.
    pub fn reply(&self, state: CancelState) -> Self {
        Self {
            version: Version::Only,
            message: CancelType::Only,
            activation_id: self.activation_id.clone(),
            attempt_id: self.attempt_id.clone(),
            cancel: match state {
                CancelState::Requested => Cancel::Requested {},
                CancelState::Acknowledged => Cancel::Acknowledged {},
                CancelState::Uncertain => Cancel::Uncertain {},
            },
        }
    }
}
