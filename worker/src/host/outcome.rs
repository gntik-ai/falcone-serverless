//! Typed outcomes of one guest execution, and the bounded termination fallback.
use super::Host;
use std::io;
use std::time::{Duration, Instant};
use wasmtime::Trap;

/// How an execution ended. `Completed` is the only success; every limit, cancellation
/// and uncertainty is its own variant so none can be mistaken for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    /// The guest deadline interrupted the guest (epoch interruption).
    GuestInterrupted,
    /// The guest-memory limit denied a growth and the guest trapped.
    GuestMemoryLimit,
    /// A host call was cancelled.
    Cancelled,
    /// A host call outlived its deadline after the binding started: the platform effect
    /// may or may not have happened.
    Uncertain,
    /// The execution ignored cancellation and the termination fallback ended it.
    Terminated,
    /// Any other guest trap.
    Trapped,
}

/// Classify the result of a guest call on `host`'s store. A host call that did not
/// complete cleanly keeps the result from ever reading as `Completed`.
pub fn classify<T>(host: &Host, result: &wasmtime::Result<T>) -> Outcome {
    let not_confirmed = if host.uncertain_host_call() {
        Some(Outcome::Uncertain)
    } else if host.cancellation_observed() {
        Some(Outcome::Cancelled)
    } else {
        None
    };
    match result {
        Ok(_) => not_confirmed.unwrap_or(Outcome::Completed),
        Err(error) if matches!(error.downcast_ref::<Trap>(), Some(Trap::Interrupt)) => {
            Outcome::GuestInterrupted
        }
        Err(_) if host.guest_memory_limit_hit() => Outcome::GuestMemoryLimit,
        Err(_) => not_confirmed.unwrap_or(Outcome::Trapped),
    }
}

/// An execution that can be asked to stop and, failing that, forcibly ended.
pub trait Execution {
    /// Ask the execution to stop. It is free to ignore this.
    fn request_cancel(&mut self) -> io::Result<()>;
    /// True once the execution has ended and been reaped. Never blocks.
    fn has_ended(&mut self) -> io::Result<bool>;
    /// End the execution by force and reap it. Must not wait on the execution's consent.
    fn terminate(&mut self) -> io::Result<()>;
}

/// Starts executions in a form that can be forcibly ended. The process model is not
/// decided (ADR 0004 is Proposed); implementations here are qualification-only.
pub trait TerminationFallback {
    type Running: Execution;
    fn start(&self, fixture: &str) -> io::Result<Self::Running>;
}

#[derive(Debug, PartialEq, Eq)]
pub enum Ended {
    /// The execution stopped on its own after the cancellation request.
    Stopped { elapsed: Duration },
    /// The fallback had to end it.
    Terminated { elapsed: Duration },
    /// It ended, but only after the budget: the fallback is not bounded.
    OverBudget { elapsed: Duration },
}

impl Ended {
    pub fn outcome(&self) -> Option<Outcome> {
        match self {
            Ended::Stopped { .. } => Some(Outcome::Cancelled),
            Ended::Terminated { .. } => Some(Outcome::Terminated),
            Ended::OverBudget { .. } => None,
        }
    }
}

/// Request cancellation, give the execution half of `budget` to stop, then terminate it.
/// Everything, including the reap, is measured against `budget` from the request.
pub fn end_execution(execution: &mut dyn Execution, budget: Duration) -> io::Result<Ended> {
    let started = Instant::now();
    execution.request_cancel()?;
    let grace = started + budget / 2;
    while Instant::now() < grace {
        if execution.has_ended()? {
            return Ok(within(started, budget, false));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    if execution.has_ended()? {
        return Ok(within(started, budget, false));
    }
    execution.terminate()?;
    Ok(within(started, budget, true))
}

fn within(started: Instant, budget: Duration, terminated: bool) -> Ended {
    let elapsed = started.elapsed();
    if elapsed > budget {
        Ended::OverBudget { elapsed }
    } else if terminated {
        Ended::Terminated { elapsed }
    } else {
        Ended::Stopped { elapsed }
    }
}
