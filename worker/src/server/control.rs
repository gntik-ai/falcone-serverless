//! The control listener's operations: cancel and execution update.

use super::{Finish, State, error_body};
use crate::api::{self, Malformed};

type Reply = (u16, Vec<u8>);

fn malformed(error: Malformed) -> Reply {
    (
        400,
        serde_json::json!({ "error": "malformed_request", "message": error.to_string() })
            .to_string()
            .into_bytes(),
    )
}

/// `POST /v1/cancel`: interrupts the guest of a running execution.
///
/// The answer is `acknowledged` once the execution has ended after the interruption, and
/// `uncertain` when it has not ended within `cancel_wait` (for example a blocked host call, which
/// RUN-02 bounds) or when a mutating host call may still commit. An execution that is not running
/// answers 404 `execution_not_found`.
pub fn cancel(state: &State, body: &[u8]) -> Reply {
    let message = match api::parse_cancel(body) {
        Ok(message) => message,
        Err(error) => return malformed(error),
    };
    if !message.is_requested() {
        return (400, error_body("unexpected_cancel_state"));
    }
    let Some(running) = state.running_for(message.activation(), message.attempt()) else {
        return (404, error_body("execution_not_found"));
    };
    running
        .cancel
        .store(true, std::sync::atomic::Ordering::Release);
    let acknowledged = running.wait(state.settings.cancel_wait) == Some(Finish::Acknowledged);
    match serde_json::to_vec(&message.answer(acknowledged)) {
        Ok(answer) => (200, answer),
        Err(_) => (500, error_body("internal")),
    }
}

/// `POST /v1/update`: records an execution update (heartbeat, progress or checkpoint
/// reference) for a running execution. Sequences are strictly increasing; the worker keeps only
/// the latest and a stale or repeated sequence is refused.
pub fn update(state: &State, body: &[u8]) -> Reply {
    let message = match api::parse_update(body) {
        Ok(message) => message,
        Err(error) => return malformed(error),
    };
    let Some(running) = state.running_for(message.activation(), message.attempt()) else {
        return (404, error_body("execution_not_found"));
    };
    let sequence = message.sequence();
    let earlier = running
        .last_update
        .fetch_max(sequence, std::sync::atomic::Ordering::AcqRel);
    if earlier >= sequence {
        return (409, error_body("stale_sequence"));
    }
    (
        202,
        serde_json::json!({ "accepted": true, "sequence": sequence })
            .to_string()
            .into_bytes(),
    )
}
