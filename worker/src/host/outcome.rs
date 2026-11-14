//! Invocation outcomes supplement the unchanged WIT host-error variant.
use super::Host;
use wasmtime as engine;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvocationOutcome {
    Success,
    GuestInterrupted,
    GuestMemoryLimit,
    BufferLimit,
    Cancelled,
    Uncertain,
    /// The revision's `fuel` budget ran out.
    FuelExhausted,
    /// The call after `max_host_calls` was refused before it reached the host.
    HostCallBudget,
    Failed,
}

/// A detached mutation may still commit. Never let a later trap erase that fact.
pub fn classify(host: &Host, result: Result<(), &engine::Error>) -> InvocationOutcome {
    if host.uncertain_host_call() {
        return InvocationOutcome::Uncertain;
    }
    if host.memory_limited() {
        return InvocationOutcome::GuestMemoryLimit;
    }
    if result.is_err_and(|error| {
        matches!(
            error.downcast_ref::<engine::Trap>(),
            Some(engine::Trap::Interrupt)
        )
    }) {
        return InvocationOutcome::GuestInterrupted;
    }
    // A call that started at or after the deadline is a timeout even when it would also
    // have exceeded `max_host_calls`: the deadline is checked before the entry is counted.
    if host.deadline_refused_call() {
        return InvocationOutcome::GuestInterrupted;
    }
    if result.is_err_and(|error| {
        matches!(
            error.downcast_ref::<engine::Trap>(),
            Some(engine::Trap::OutOfFuel)
        )
    }) {
        return InvocationOutcome::FuelExhausted;
    }
    if host.host_call_budget_exceeded() {
        return InvocationOutcome::HostCallBudget;
    }
    if host.cancelled_host_call() {
        return InvocationOutcome::Cancelled;
    }
    if host.buffer_limited() {
        return InvocationOutcome::BufferLimit;
    }
    if result.is_ok() {
        InvocationOutcome::Success
    } else {
        InvocationOutcome::Failed
    }
}
