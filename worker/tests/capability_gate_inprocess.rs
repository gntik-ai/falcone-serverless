//! WAS-02.t4 in-process qualification: the same scenarios the pod runs, driven through the
//! real Wasmtime gate with mock bindings and local listeners. A sandbox that forbids local
//! sockets (EPERM) reports "NOT RUN" for the socket-bound scenarios; PR CI runs them.

#[path = "../src/qualification/capability_gates.rs"]
mod capability_gates;

use capability_gates::{Failure, Fixtures, Verdict};
use std::io::Write;
use std::path::Path;

fn fixtures() -> Fixtures {
    Fixtures {
        allowed: include_str!("../src/qualification/gate_fixtures/allowed.wat"),
        heavy_function: include_str!("../src/qualification/gate_fixtures/heavy-function.wat"),
        baseline_only: include_str!("../src/qualification/gate_fixtures/baseline-only.wat"),
        imports_wasi_filesystem: include_str!(
            "../src/qualification/gate_fixtures/imports-wasi-filesystem.wat"
        ),
        imports_wasi_sockets: include_str!(
            "../src/qualification/gate_fixtures/imports-wasi-sockets.wat"
        ),
        imports_wasi_environment: include_str!(
            "../src/qualification/gate_fixtures/imports-wasi-environment.wat"
        ),
        ungranted_storage: include_str!("../src/qualification/gate_fixtures/ungranted-storage.wat"),
        storage: include_str!("../src/qualification/gate_fixtures/storage.wat"),
        events: include_str!("../src/qualification/gate_fixtures/events.wat"),
        workflows: include_str!("../src/qualification/gate_fixtures/workflows.wat"),
        http: include_str!("../src/qualification/gate_fixtures/http.wat"),
        sinks: include_str!("../src/qualification/gate_fixtures/sinks.wat"),
        logging: include_str!("../src/qualification/gate_fixtures/logging.wat"),
    }
}

fn verdict(name: &str, result: Verdict) {
    match result {
        Ok(()) => {}
        // Written to the real stderr handle so the libtest capture does not hide it.
        Err(Failure::NotRun(reason)) => {
            let _ = writeln!(
                std::io::stderr(),
                "NOT RUN: {name}: {reason}, PR CI runs them"
            );
        }
        Err(Failure::Failed(reason)) => panic!("{name}: {reason}"),
    }
}

#[test]
fn wasi_imports_fail_instantiation_and_never_start() {
    verdict(
        "wasi imports",
        capability_gates::wasi_imports_are_denied(&fixtures()),
    );
}

#[test]
fn a_declared_interface_without_a_grant_fails_instantiation() {
    verdict(
        "ungranted",
        capability_gates::ungranted_interface_is_denied(&fixtures()),
    );
}

#[test]
fn storage_is_bound_to_the_granted_prefix() {
    verdict(
        "storage",
        capability_gates::storage_is_scope_bound(&fixtures()),
    );
}

#[test]
fn events_and_workflows_are_bound_to_the_grant() {
    verdict(
        "events and workflows",
        capability_gates::events_and_workflows_are_scope_bound(&fixtures()),
    );
}

#[test]
fn http_is_allow_listed_and_refuses_forbidden_targets() {
    verdict("http", capability_gates::http_is_allow_listed(&fixtures()));
}

#[test]
fn dns_rebinding_cannot_steer_the_connection() {
    verdict(
        "rebinding",
        capability_gates::dns_rebinding_is_refused(&fixtures()),
    );
}

#[test]
fn limits_and_deadlines_are_enforced_independently_of_fuel() {
    verdict(
        "limits",
        capability_gates::limits_and_deadlines_hold(&fixtures()),
    );
}

#[cfg(target_os = "linux")]
#[test]
fn compile_budgets_are_enforced() {
    let child = Path::new(env!("CARGO_BIN_EXE_artifact-compile-child"));
    verdict(
        "compile budget",
        capability_gates::compile_budget_is_enforced(&fixtures(), child),
    );
}
