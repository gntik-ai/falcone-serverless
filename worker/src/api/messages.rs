//! Execution updates (heartbeat, progress, checkpoint reference) and cancellation.

use super::types::{Count, Id, Positive, Text, Version};
use super::wire::{constant, present, redacted};
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
    pub fn activation(&self) -> &str {
        self.activation_id.as_str()
    }

    pub fn attempt(&self) -> &str {
        self.attempt_id.as_str()
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

impl CancelMessage {
    pub fn activation(&self) -> &str {
        self.activation_id.as_str()
    }

    pub fn attempt(&self) -> &str {
        self.attempt_id.as_str()
    }

    /// Only the dispatcher sends `requested`; the other states are the worker's answers.
    pub fn is_requested(&self) -> bool {
        matches!(self.cancel, Cancel::Requested {})
    }

    /// The worker's answer: `uncertain` when a host call may still commit or the guest did
    /// not stop, otherwise `acknowledged`. It claims no reversal of external side effects.
    pub fn answer(&self, uncertain: bool) -> Self {
        Self {
            version: Version::Only,
            message: CancelType::Only,
            activation_id: self.activation_id.clone(),
            attempt_id: self.attempt_id.clone(),
            cancel: if uncertain {
                Cancel::Uncertain {}
            } else {
                Cancel::Acknowledged {}
            },
        }
    }
}
