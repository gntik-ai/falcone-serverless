//! Execution updates (heartbeat, progress, checkpoint reference) and cancellation.

use super::{
    error::Malformed,
    from_bytes,
    types::{Count, Digest, Name, Percent, Presence, V1, present},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateKind {
    Heartbeat,
    Progress,
    Checkpoint,
}

/// One update for an attempt. `percent` and `message` belong to progress; `checkpoint_ref`
/// and `digest` to a checkpoint; a heartbeat has none of them. Absent members are absent,
/// never `null`.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    pub contract_version: V1,
    pub activation_id: Name,
    pub attempt_id: Name,
    pub sequence: Count,
    pub kind: UpdateKind,
    #[serde(
        default = "Presence::absent",
        deserialize_with = "present",
        skip_serializing_if = "Presence::is_absent"
    )]
    pub percent: Presence<Percent>,
    #[serde(
        default = "Presence::absent",
        deserialize_with = "present",
        skip_serializing_if = "Presence::is_absent"
    )]
    pub message: Presence<Name>,
    #[serde(
        default = "Presence::absent",
        deserialize_with = "present",
        skip_serializing_if = "Presence::is_absent"
    )]
    pub checkpoint_ref: Presence<Name>,
    #[serde(
        default = "Presence::absent",
        deserialize_with = "present",
        skip_serializing_if = "Presence::is_absent"
    )]
    pub digest: Presence<Digest>,
}

impl Update {
    fn check(&self) -> Result<(), Malformed> {
        let percent = self.percent.value()?;
        let message = self.message.value()?;
        let checkpoint = self.checkpoint_ref.value()?;
        let digest = self.digest.value()?;
        let shape = match self.kind {
            UpdateKind::Heartbeat => {
                percent.is_none() && message.is_none() && checkpoint.is_none() && digest.is_none()
            }
            UpdateKind::Progress => checkpoint.is_none() && digest.is_none(),
            UpdateKind::Checkpoint => {
                percent.is_none() && message.is_none() && checkpoint.is_some() && digest.is_some()
            }
        };
        if shape {
            Ok(())
        } else {
            Err(Malformed::Rule("update"))
        }
    }
}

pub fn parse_update(bytes: &[u8]) -> Result<Update, Malformed> {
    let update: Update = from_bytes(bytes)?;
    update.check()?;
    Ok(update)
}

/// `requested` goes to the worker; `acknowledged` and `uncertain` come back. `uncertain`
/// means the worker cannot confirm the attempt stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelState {
    Requested,
    Acknowledged,
    Uncertain,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Cancel {
    pub contract_version: V1,
    pub activation_id: Name,
    pub attempt_id: Name,
    pub state: CancelState,
}

pub fn parse_cancel(bytes: &[u8]) -> Result<Cancel, Malformed> {
    from_bytes(bytes)
}
