//! What the execution service reads from an admitted request: the limits it runs under.
//!
//! The limits are the minimum of what the request, the revision and the worker allow, and a
//! budget the worker cannot honour is refused, never replaced by a default.

use super::refusal::{Code, Reason, Refusal};
use super::request::ExecuteRequest;
use super::wire::redacted;
use std::time::Duration;

/// The worker's own ceilings, from its runtime configuration and its engine contract.
#[derive(Clone, Copy)]
pub struct WorkerLimits {
    /// `max_memory_bytes` of the runtime configuration.
    pub max_memory_bytes: u64,
    /// The engine's compatibility ceiling for guest memory.
    pub compatibility_memory_bytes: u64,
    /// `max_wall_clock_ms` of the runtime configuration.
    pub max_wall_clock: Duration,
    /// The Wasmtime version this worker embeds.
    pub engine_version: &'static str,
}
redacted!(WorkerLimits);

/// The limits one execution runs under.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ExecutionLimits {
    /// Guest linear memory: the minimum of the revision budget and both worker ceilings.
    pub memory_bytes: u64,
    pub fuel: u64,
    pub max_host_calls: u64,
    /// The shortest of the request deadline, the revision budget and the worker wall clock.
    pub timeout: Duration,
}
redacted!(ExecutionLimits);

fn invalid(reason: Reason) -> Refusal {
    Refusal::new(Code::InvalidRequest, reason)
}

impl ExecuteRequest {
    /// The limits of this attempt, `now_unix_millis` being the admission instant. A budget above
    /// a worker ceiling, or a projection built for another engine, is refused before anything
    /// is compiled.
    pub fn execution_limits(
        &self,
        worker: &WorkerLimits,
        now_unix_millis: u64,
    ) -> Result<ExecutionLimits, Refusal> {
        let projection = self.projection();
        if projection.engine_version() != worker.engine_version {
            return Err(invalid(Reason::EngineVersionMismatch));
        }
        let budgets = projection.budgets();
        let memory_ceiling = worker
            .max_memory_bytes
            .min(worker.compatibility_memory_bytes);
        if budgets.memory_bytes() > memory_ceiling {
            return Err(invalid(Reason::BudgetAboveWorkerCeiling));
        }
        let request =
            Duration::from_millis(self.deadline_unix_millis().saturating_sub(now_unix_millis));
        let timeout = request
            .min(Duration::from_millis(budgets.timeout_ms()))
            .min(worker.max_wall_clock);
        Ok(ExecutionLimits {
            memory_bytes: budgets
                .memory_bytes()
                .min(worker.max_memory_bytes)
                .min(worker.compatibility_memory_bytes),
            fuel: budgets.fuel(),
            max_host_calls: budgets.max_host_calls(),
            timeout,
        })
    }
}
