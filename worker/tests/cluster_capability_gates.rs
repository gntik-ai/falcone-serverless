//! Operator-only restricted WAS-02 qualification; no live access in ordinary tests.
#![cfg(target_os = "linux")]

#[path = "support/image.rs"]
pub mod image;
#[path = "support/kubeconfig.rs"]
pub mod kubeconfig;
#[path = "support/namespace.rs"]
pub mod namespace;

const QUALIFICATION_PREFIX: &str = "was02-gates";
const QUALIFICATION_FLAG: &str = "--capability-gates";
const QUALIFICATION_CHECK: &str = "capability_gates";
/// This qualification reports no per-case records; the aggregate record is enough.
const REQUIRED_CASES: &[&str] = &[];
const CLUSTER_TEST: &str = "cluster_capability_gates";

include!("support/was02_cluster.rs");

#[test]
#[ignore = "UNVERIFIED: FS_TEST_CLUSTER unset"]
fn cluster_capability_gates() -> Result<(), String> {
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
            "--capability-gates"
        ])
    );
}
