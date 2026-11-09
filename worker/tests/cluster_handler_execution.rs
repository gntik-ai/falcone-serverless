//! Operator-only restricted WAS-01 handler execution qualification; no live access in ordinary tests.
#![cfg(target_os = "linux")]

#[path = "support/image.rs"]
pub mod image;
#[path = "support/kubeconfig.rs"]
pub mod kubeconfig;
#[path = "support/namespace.rs"]
pub mod namespace;

const QUALIFICATION_PREFIX: &str = "was01-handler";
const QUALIFICATION_FLAG: &str = "--handler-execution";
const QUALIFICATION_CHECK: &str = "handler_execution";
const CLUSTER_TEST: &str = "cluster_handler_execution";

const REQUIRED_CASES: &[&str] = &[
    "successful_handler_call",
    "host_capability",
    "host_import_limits",
    "signature_before_host_call",
    "utf16_before_host_call",
    "latin1_before_host_call",
    "host_to_guest_limits",
    "guest_to_host_limits",
    "guest_to_host_allocations",
    "timeout_isolation",
    "blocked_host_call_deadline",
    "cancelled_success",
    "cancelled_error",
    "cancelled_expired_before_guest_call",
    "expired_success",
    "expired_error",
];

include!("support/was02_cluster.rs");

#[test]
#[ignore = "UNVERIFIED: FS_TEST_CLUSTER unset"]
fn cluster_handler_execution() -> Result<(), String> {
    live_qualification()
}

#[test]
fn pod_selects_the_requested_qualification() {
    let plan = NamespacePlan::new("was02", &NamespaceMode::Create).unwrap();
    let image = format!("registry.invalid/compiler@sha256:{}", "a".repeat(64));
    let pod = pod_manifest(&plan, &image).unwrap();
    assert_eq!(
        pod["spec"]["containers"][0]["command"],
        serde_json::json!([
            "/usr/local/bin/compile-cache-qualification",
            "--handler-execution"
        ])
    );
}

#[test]
fn pod_logs_require_every_handler_security_case() {
    let cases = REQUIRED_CASES;
    let aggregate = serde_json::json!({"check":"handler_execution","status":"passed"}).to_string();
    let records: Vec<String> = cases
        .iter()
        .map(|case| serde_json::json!({"case":case,"status":"passed"}).to_string())
        .collect();
    let complete = format!("{}\n{aggregate}", records.join("\n"));
    assert!(qualification_logs_passed(&complete));
    assert!(!qualification_logs_passed(&records.join("\n")));
    assert!(!qualification_logs_passed(&aggregate));
    for missing in 0..records.len() {
        let remaining: Vec<_> = records
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != missing)
            .map(|(_, record)| record.as_str())
            .collect();
        assert!(
            !qualification_logs_passed(&format!("{}\n{aggregate}", remaining.join("\n"))),
            "missing {}",
            cases[missing]
        );
        assert!(
            !qualification_logs_passed(&complete.replace(
                &records[missing],
                &records[missing].replace("passed", "failed")
            )),
            "failed {}",
            cases[missing]
        );
    }
}
