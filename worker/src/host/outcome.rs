//! Invocation outcomes supplement the unchanged WIT host-error variant.
use super::Host;
use wasmtime as engine;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvocationOutcome {
    Success,
    GuestInterrupted,
    GuestMemoryLimit,
    FuelExhausted,
    HostCallBudget,
    BufferLimit,
    Cancelled,
    Uncertain,
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
    // The deadline outranks every budget: a call refused because it started at or after the
    // deadline is a timeout even when it would also have been over the host-call budget.
    let trap =
        |kind| result.is_err_and(|error| error.downcast_ref::<engine::Trap>() == Some(&kind));
    if trap(engine::Trap::Interrupt) || host.deadline_refused() {
        return InvocationOutcome::GuestInterrupted;
    }
    if host.cancelled_host_call() {
        return InvocationOutcome::Cancelled;
    }
    if trap(engine::Trap::OutOfFuel) {
        return InvocationOutcome::FuelExhausted;
    }
    if host.host_call_budget_exceeded() {
        return InvocationOutcome::HostCallBudget;
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
