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
    pub fn activation_id(&self) -> &Id {
        &self.activation_id
    }

    pub fn attempt_id(&self) -> &Id {
        &self.attempt_id
    }

    /// Whether this is the dispatcher's request; the other states are the worker's replies.
    pub fn is_requested(&self) -> bool {
        matches!(self.cancel, Cancel::Requested {})
    }

    /// The worker's reply to this request: `acknowledged`, or `uncertain` when the worker cannot
    /// say that the guest has stopped.
    pub fn reply(&self, acknowledged: bool) -> Self {
        Self {
            version: Version::Only,
            message: CancelType::Only,
            activation_id: self.activation_id.clone(),
            attempt_id: self.attempt_id.clone(),
            cancel: if acknowledged {
                Cancel::Acknowledged {}
            } else {
                Cancel::Uncertain {}
            },
        }
    }
}

/// What a guest last reported through the progress interface.
#[derive(Clone, PartialEq, Eq)]
pub enum Latest {
    Progress { completed: u64, total: Option<u64> },
    Checkpoint(String),
}

impl UpdateMessage {
    pub fn activation_id(&self) -> &Id {
        &self.activation_id
    }

    pub fn attempt_id(&self) -> &Id {
        &self.attempt_id
    }

    /// Whether this is a heartbeat, the update a dispatcher polls a running attempt with.
    pub fn is_heartbeat(&self) -> bool {
        matches!(self.update, Update::Heartbeat {})
    }

    /// The worker's reply to a heartbeat: the latest progress or checkpoint reference of the
    /// attempt, or a heartbeat when it reported none. `reports` is how many reports the attempt
    /// has made; the reply's sequence never falls below it or below the request's. `None` when
    /// the report is not a valid update (a checkpoint reference over the text bound).
    pub fn reply(&self, latest: Option<Latest>, reports: u64) -> Option<Self> {
        const MAX_WHOLE: u64 = 9_007_199_254_740_991;
        let update = match latest {
            None => serde_json::json!({"kind": "heartbeat"}),
            Some(Latest::Progress { completed, total }) => {
                let completed = completed.min(MAX_WHOLE);
                let mut update = serde_json::json!({"kind": "progress", "completed": completed});
                if let Some(total) = total.filter(|&total| total >= completed.max(1)) {
                    update["total"] = total.min(MAX_WHOLE).into();
                }
                update
            }
            Some(Latest::Checkpoint(reference)) => {
                serde_json::json!({"kind": "checkpoint", "checkpoint_ref": reference})
            }
        };
        let sequence = self.sequence.get().max(reports).clamp(1, MAX_WHOLE);
        let message: Self = serde_json::from_value(serde_json::json!({
            "version": "1",
            "type": "update",
            "activation_id": self.activation_id.as_str(),
            "attempt_id": self.attempt_id.as_str(),
            "sequence": sequence,
            "update": update,
        }))
        .ok()?;
        message.check().ok()?;
        Some(message)
    }
}
