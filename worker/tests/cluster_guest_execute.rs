//! Operator-only restricted qualification of the guest execute path (WAS-01 #277). Runs
//! `compile-cache-qualification --guest-execute` in the restricted fs-test pod on the
//! shared cluster; no live access in ordinary tests. The in-process twins of every case
//! are in `guest_execute.rs`.
#![cfg(target_os = "linux")]

pub use falcone_serverless_worker::{host, limits};
#[path = "../src/qualification/guest_execute_cases.rs"]
pub mod cases;
#[path = "../src/qualification/handler_fixtures.rs"]
pub mod handler_fixtures;
#[path = "support/image.rs"]
pub mod image;
#[path = "support/kubeconfig.rs"]
pub mod kubeconfig;
#[path = "support/namespace.rs"]
pub mod namespace;

const QUALIFICATION_PREFIX: &str = "was01-exec";
const QUALIFICATION_FLAG: &str = "--guest-execute";
const QUALIFICATION_CHECK: &str = "guest_execute";
const CLUSTER_TEST: &str = "cluster_guest_execute";
/// One case per security-relevant criterion, listed independently of the runner: the pod
/// passes only if each reports `passed`, so none can be dropped silently.
const REQUIRED_CASES: &[&str] = &[
    "successful_handler_call",
    "size_ceilings",
    "limits_are_configurable",
    "timeout_isolation",
    "cancellation_precedence",
    "deadline_precedence",
    "host_call_timeout_is_not_cancellation",
    "oversized_guest_data",
    "capability_allowances_hold",
    "bounded_errors",
    "signature_rejection",
    "trusted_compile_path",
];

include!("support/was02_cluster.rs");

#[test]
#[ignore = "UNVERIFIED: FS_TEST_CLUSTER unset"]
fn cluster_guest_execute() -> Result<(), String> {
    live_qualification()
}

#[test]
fn pod_selects_the_requested_qualification() {
    let plan = NamespacePlan::new("was01", &NamespaceMode::Create).unwrap();
    let image = format!("registry.invalid/compiler@sha256:{}", "a".repeat(64));
    let pod = pod_manifest(&plan, &image).unwrap();
    assert_eq!(
        pod["spec"]["containers"][0]["command"],
        serde_json::json!([
            "/usr/local/bin/compile-cache-qualification",
            "--guest-execute"
        ])
    );
}

#[test]
fn every_required_case_is_registered_in_the_runner_table() {
    // The runner iterates this table, so registration is what the pod runs and reports.
    // The trusted compile path is the runner's own step, not a table entry.
    let mut registered: Vec<&str> = cases::CASES.iter().map(|(name, _)| *name).collect();
    registered.push("trusted_compile_path");
    registered.sort_unstable();
    let mut required = REQUIRED_CASES.to_vec();
    required.sort_unstable();
    assert_eq!(registered, required);
    // And the runner is reachable through the flag the pod uses.
    assert!(
        include_str!("../src/qualification/compile_cache_runner.rs")
            .contains("Some(\"--guest-execute\") => return guest_execute_runner::main()")
    );
}
