//! Operator-only restricted qualification of the guest execute path (WAS-01 #277). Runs
//! `compile-cache-qualification --guest-execute` in the restricted fs-test pod on the
//! shared cluster; no live access in ordinary tests. The in-process twins of every case
//! are in `guest_execute.rs`.
#![cfg(target_os = "linux")]

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
fn every_required_case_exists_in_the_runner() {
    let cases = include_str!("../src/qualification/guest_execute_cases.rs");
    let runner = include_str!("../src/qualification/guest_execute_runner.rs");
    for case in REQUIRED_CASES {
        let quoted = format!("\"{case}\"");
        assert!(
            cases.contains(&quoted) || runner.contains(&quoted),
            "runner does not run {case}"
        );
    }
    // And the runner is reachable through the flag the pod uses.
    assert!(
        include_str!("../src/qualification/compile_cache_runner.rs")
            .contains("Some(\"--guest-execute\") => return guest_execute_runner::main()")
    );
}
