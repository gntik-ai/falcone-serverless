#![cfg(target_os = "linux")]
//! WAS-02.t4 ac8: the compile-budget fixture harness kills an over-budget child compile with a
//! limit error and keeps no output; a within-budget compile succeeds. The compile runs in the
//! trusted `artifact-compile-child` process under the supervisor's OS limits.

use falcone_serverless_worker::compile_budget::{
    BudgetOutcome, CompileBudget, compile_within_budget,
};
use std::path::Path;
use std::time::Duration;

const CHILD: &str = env!("CARGO_BIN_EXE_artifact-compile-child");

fn small() -> Vec<u8> {
    wat::parse_str(include_str!("fixtures/was-02/allowed.wat")).unwrap()
}

/// Deterministically expanded, never instantiated: compile time and memory scale with it.
fn heavy() -> Vec<u8> {
    let body = "i32.const 3 i32.mul i32.const 1 i32.add\n".repeat(8192);
    let function =
        include_str!("fixtures/was-02/heavy-function.wat").replace(";; OPERATIONS", &body);
    wat::parse_str(format!(
        "(component (core module {}))",
        function.repeat(128)
    ))
    .unwrap()
}

fn budget() -> CompileBudget {
    CompileBudget {
        input_bytes: 64 * 1024 * 1024,
        cpu_seconds: 20,
        // RLIMIT_AS is virtual address space: generous for the small control.
        memory_bytes: 2 * 1024 * 1024 * 1024,
        wall: Duration::from_secs(60),
    }
}

fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn a_compile_within_budget_succeeds() {
    let report = compile_within_budget(Path::new(CHILD), &small(), budget());
    assert_eq!(report.outcome, BudgetOutcome::WithinBudget);
    assert!(report.child_reaped);
}

#[test]
fn a_compile_over_the_cpu_budget_is_killed_with_a_limit_error() {
    let mut tight = budget();
    tight.cpu_seconds = 1;
    let cwd = std::env::current_dir().unwrap();
    let before = listing(&cwd);
    let report = compile_within_budget(Path::new(CHILD), &heavy(), tight);
    assert_eq!(report.outcome, BudgetOutcome::CpuLimitExceeded);
    assert!(report.child_reaped, "the killed child must be reaped");
    assert_eq!(
        listing(&cwd),
        before,
        "an over-budget compile leaves no output"
    );
    // The harness is reusable: the next in-budget compile succeeds.
    let control = compile_within_budget(Path::new(CHILD), &small(), tight);
    assert_eq!(control.outcome, BudgetOutcome::WithinBudget);
}

#[test]
fn a_compile_over_the_memory_budget_is_killed_with_a_limit_error() {
    let mut tight = budget();
    // Test-only address-space budget; the small control fits with margin.
    tight.memory_bytes = 64 * 1024 * 1024;
    let cwd = std::env::current_dir().unwrap();
    let before = listing(&cwd);
    let report = compile_within_budget(Path::new(CHILD), &heavy(), tight);
    assert_eq!(report.outcome, BudgetOutcome::MemoryLimitExceeded);
    assert!(report.child_reaped);
    assert_eq!(listing(&cwd), before);
    let control = compile_within_budget(Path::new(CHILD), &small(), tight);
    assert_eq!(control.outcome, BudgetOutcome::WithinBudget);
}

#[test]
fn rejected_input_never_starts_a_compile() {
    let report = compile_within_budget(Path::new(CHILD), b"not wasm", budget());
    assert_eq!(
        report.outcome,
        BudgetOutcome::Rejected("precompiled_rejected")
    );
    assert!(!report.child_reaped, "no child was spawned");
}
