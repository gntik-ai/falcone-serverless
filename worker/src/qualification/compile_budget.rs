//! Compile-budget fixture harness (WAS-02.t4). It reuses the artifact supervisor: the compile
//! runs in the trusted compile-only child under CPU-time and address-space limits, the
//! supervisor kills and reaps the child on overrun, and the harness keeps no compile output:
//! the only product is the verdict.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use crate::artifact::supervisor::{CompileLimits, Supervisor};

#[derive(Clone, Copy, Debug)]
pub struct CompileBudget {
    pub input_bytes: usize,
    pub cpu_seconds: u64,
    /// RLIMIT_AS: virtual address space, not resident memory.
    pub memory_bytes: u64,
    pub wall: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BudgetOutcome {
    WithinBudget,
    CpuLimitExceeded,
    MemoryLimitExceeded,
    DeadlineExceeded,
    /// Refused before a child was spawned, or failed for another reason (the error code).
    Rejected(&'static str),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BudgetReport {
    pub outcome: BudgetOutcome,
    /// True when a child was spawned and has been reaped by the time this returns.
    pub child_reaped: bool,
}

/// Compiles `bytes` in the child at `child` (a trusted platform binary) under `budget`.
pub fn compile_within_budget(child: &Path, bytes: &[u8], budget: CompileBudget) -> BudgetReport {
    let mut supervisor = Supervisor::new(child);
    let limits = CompileLimits {
        input_bytes: budget.input_bytes,
        cpu_seconds: budget.cpu_seconds,
        memory_bytes: budget.memory_bytes,
        wall_clock_deadline: Instant::now() + budget.wall,
    };
    let cancelled = AtomicBool::new(false);
    let outcome = match supervisor.compile(bytes, limits, &cancelled) {
        Ok(_) => BudgetOutcome::WithinBudget,
        Err(error) => match error.code {
            "compile_cpu_limit_exceeded" => BudgetOutcome::CpuLimitExceeded,
            "compile_memory_limit_exceeded" => BudgetOutcome::MemoryLimitExceeded,
            "compile_deadline_exceeded" => BudgetOutcome::DeadlineExceeded,
            code => BudgetOutcome::Rejected(code),
        },
    };
    let child_reaped =
        supervisor.spawn_count() > 0 && matches!(supervisor.last_child_status(), Ok(Some(_)));
    BudgetReport {
        outcome,
        child_reaped,
    }
}
