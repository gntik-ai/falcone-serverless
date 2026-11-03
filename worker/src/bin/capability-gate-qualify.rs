//! Standalone WAS-02.t4 qualification, run inside the restricted fs-test pod by
//! `worker/tests/cluster_capability_gates.rs`; never invoked by the worker service. It prints
//! one `WAS02 <scenario> PASS|FAIL|NOT_RUN` record per scenario and exits non-zero on any
//! failure or on a scenario that could not run.
#[path = "../qualification/capability_gates.rs"]
mod capability_gates;

use capability_gates::{Failure, Fixtures, Verdict};
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

fn fixtures() -> Fixtures {
    Fixtures {
        allowed: include_str!("../../tests/fixtures/was-02/allowed.wat"),
        heavy_function: include_str!("../../tests/fixtures/was-02/heavy-function.wat"),
        baseline_only: include_str!("../../tests/fixtures/was-02/baseline-only.wat"),
        imports_wasi_filesystem: include_str!(
            "../../tests/fixtures/was-02/imports-wasi-filesystem.wat"
        ),
        imports_wasi_sockets: include_str!("../../tests/fixtures/was-02/imports-wasi-sockets.wat"),
        imports_wasi_environment: include_str!(
            "../../tests/fixtures/was-02/imports-wasi-environment.wat"
        ),
        ungranted_storage: include_str!("../../tests/fixtures/was-02/ungranted-storage.wat"),
        storage: include_str!("../../tests/fixtures/was-02/storage.wat"),
        events: include_str!("../../tests/fixtures/was-02/events.wat"),
        workflows: include_str!("../../tests/fixtures/was-02/workflows.wat"),
        http: include_str!("../../tests/fixtures/was-02/http.wat"),
    }
}

/// The compile child is a trusted platform binary shipped next to this one.
fn compile_child() -> Option<PathBuf> {
    let path = std::env::current_exe()
        .ok()?
        .with_file_name("artifact-compile-child");
    path.is_file().then_some(path)
}

fn main() -> ExitCode {
    let fx = fixtures();
    let child = compile_child();
    let mut scenarios: Vec<(&str, Verdict)> = vec![
        (
            "wasi-imports-denied",
            capability_gates::wasi_imports_are_denied(&fx),
        ),
        (
            "ungranted-interface-denied",
            capability_gates::ungranted_interface_is_denied(&fx),
        ),
        (
            "storage-scope",
            capability_gates::storage_is_scope_bound(&fx),
        ),
        (
            "events-workflows-scope",
            capability_gates::events_and_workflows_are_scope_bound(&fx),
        ),
        (
            "http-allow-list",
            capability_gates::http_is_allow_listed(&fx),
        ),
        (
            "dns-rebinding",
            capability_gates::dns_rebinding_is_refused(&fx),
        ),
        (
            "limits-deadlines",
            capability_gates::limits_and_deadlines_hold(&fx),
        ),
    ];
    scenarios.push((
        "compile-budget",
        match &child {
            Some(child) => capability_gates::compile_budget_is_enforced(&fx, child),
            None => Err(Failure::NotRun(
                "artifact-compile-child is not next to this binary".into(),
            )),
        },
    ));
    let mut stdout = std::io::stdout().lock();
    let mut clean = true;
    for (name, verdict) in scenarios {
        let line = match verdict {
            Ok(()) => format!("WAS02 {name} PASS"),
            Err(Failure::NotRun(reason)) => {
                clean = false;
                format!("WAS02 {name} NOT_RUN {reason}")
            }
            Err(Failure::Failed(reason)) => {
                clean = false;
                format!("WAS02 {name} FAIL {reason}")
            }
        };
        if writeln!(stdout, "{line}").is_err() {
            return ExitCode::FAILURE;
        }
    }
    if clean {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
