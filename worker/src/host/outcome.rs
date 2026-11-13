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
    if host.fuel_exhausted() {
        return InvocationOutcome::FuelExhausted;
    }
    if host.host_call_budget_exceeded() {
        return InvocationOutcome::HostCallBudget;
    }
    if result.is_err_and(|error| {
        matches!(
            error.downcast_ref::<engine::Trap>(),
            Some(engine::Trap::Interrupt)
        )
    }) {
        return InvocationOutcome::GuestInterrupted;
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
