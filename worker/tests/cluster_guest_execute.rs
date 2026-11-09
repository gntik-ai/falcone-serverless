//! Operator-only handler execution, one restricted pod per security criterion.
#![cfg(target_os = "linux")]
#[path = "support/image.rs"]
pub mod image;
#[path = "support/kubeconfig.rs"]
pub mod kubeconfig;
#[path = "support/namespace.rs"]
pub mod namespace;

const QUALIFICATION_PREFIX: &str = "guest-execute";
const QUALIFICATION_FLAG: &str = "--guest-execute=echo";
const QUALIFICATION_CHECK: &str = "guest_execute";
const CLUSTER_TEST: &str = "cluster_guest_execute";
include!("support/was02_cluster.rs");

#[test]
#[ignore = "UNVERIFIED: FS_TEST_CLUSTER unset"]
fn cluster_guest_execute() -> Result<(), String> {
    if std::env::var_os("FS_TEST_CLUSTER").is_none() {
        println!("UNVERIFIED: FS_TEST_CLUSTER unset; no client call");
        return Ok(());
    }
    live_qualification()
}

fn run_case(case: &str) -> Result<(), String> {
    let image = std::env::var("FS_TEST_WAS02_IMAGE").ok();
    let Some((image, mode, configs)) = validated_inputs(
        std::env::var("FS_TEST_CLUSTER").ok().as_deref(),
        std::env::var("KUBECONFIG").ok().as_deref(),
        image.as_deref(),
        std::env::var("FS_TEST_NAMESPACE_MODE").ok().as_deref(),
        std::env::var("FS_TEST_NAMESPACE").ok().as_deref(),
    )?
    else {
        println!("UNVERIFIED: FS_TEST_CLUSTER unset; no client call");
        return Ok(());
    };
    let plan = NamespacePlan::new(QUALIFICATION_PREFIX, &mode)?;
    let mut pod = pod_manifest(&plan, image)?;
    pod["spec"]["containers"][0]["command"][1] = format!("--guest-execute={case}").into();
    let cluster = RealCluster::connect(&configs)?;
    let guard = plan.acquire(&cluster)?;
    let result = cluster.run_pod(guard.namespace(), &pod);
    guard.close()?;
    result
}
macro_rules! cluster_cases {
    ($($name:ident => $case:literal),* $(,)?) => {$(
        #[test]
        #[ignore = "UNVERIFIED: FS_TEST_CLUSTER unset"]
        fn $name() -> Result<(), String> { run_case($case) }
    )*};
}
cluster_cases! {
    import_fields => "import-fields",
    import_limit => "import-limit",
    async_utf16 => "async-utf16",
    async_latin1 => "async-latin1",
    cancel_before_deadline_success => "cancel-before-deadline-success",
    cancel_before_deadline_error => "cancel-before-deadline-error",
    post_return => "post-return",
    output_limits => "output-limits",
    params_limit => "params-limit",
    timeout_isolation => "timeout-isolation",
    cancel_success => "cancel-success",
    cancel_error => "cancel-error",
    deadline_success => "deadline-success",
    deadline_error => "deadline-error",
    expired_prepared => "expired-prepared",
    invalid_export => "invalid-export",
    wrong_signature => "wrong-signature",
    missing_export => "missing-export",
    forbidden_imports => "forbidden-imports",
    utf16 => "utf16",
    latin1 => "latin1",
    utf16_lower => "utf16-lower",
    uncertain_success => "uncertain-success",
    uncertain_error => "uncertain-error",
    uncertain_trap => "uncertain-trap",
    uncertain_cancel => "uncertain-cancel",
    uncertain_limit => "uncertain-limit",
    uncertain_deadline => "uncertain-deadline",
    non_mutating_timeout => "non-mutating-timeout",
    allocation_small_result => "allocation-small-result",
    allocation_small_error => "allocation-small-error",
    allocation_default_result => "allocation-default-result",
    allocation_default_error => "allocation-default-error",
}

#[test]
fn pod_selects_guest_execute() {
    let plan = NamespacePlan::new("guest-execute", &NamespaceMode::Create).unwrap();
    let image = format!("registry.invalid/worker@sha256:{}", "a".repeat(64));
    let pod = pod_manifest(&plan, &image).unwrap();
    assert_eq!(
        pod["spec"]["containers"][0]["command"],
        serde_json::json!([
            "/usr/local/bin/compile-cache-qualification",
            "--guest-execute=echo"
        ])
    );
}
