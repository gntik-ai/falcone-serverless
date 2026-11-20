//! Go profile qualification in a restricted pod (operator-run, `#[ignore]`, FS_TEST_CLUSTER).
//! The pod runs the prebuilt profile_go_runtime test binary over trusted-image-built
//! components: the positive workloads and the events example, with exact dependency-call
//! assertions, and it confirms that no negative fixture is built or run. The namespace is a
//! self-created and self-deleted `fs-test-*` one under the restricted posture. Results are
//! UNVERIFIED unless that run passes; with the gate unset the test skips with a reason.
#![cfg(target_os = "linux")]
#[path = "support/image.rs"]
pub mod image;
#[path = "support/kubeconfig.rs"]
pub mod kubeconfig;
#[path = "support/namespace.rs"]
pub mod namespace;

const QUALIFICATION_PREFIX: &str = "profile-go";
const QUALIFICATION_FLAG: &str = "--profile-go";
const QUALIFICATION_CHECK: &str = "profile_go";
const CLUSTER_TEST: &str = "cluster_profile_go";
const SKIP_EXPLICIT_REQUEST_WITHOUT_GATE: bool = true;
const PRODUCTION_IMAGE_ENV: Option<&str> = None;

include!("support/was02_cluster.rs");

fn profile_pod(plan: &NamespacePlan, image: &str) -> Result<serde_json::Value, String> {
    let mut pod = pod_manifest(plan, image)?;
    pod["spec"]["containers"][0]["command"] = serde_json::json!([
        "/usr/local/bin/profile-go-runtime",
        "--exact",
        "built_profile_components",
        "--ignored",
        "--nocapture"
    ]);
    pod["spec"]["containers"][0]["env"] = serde_json::json!([
        {"name":"FS_TOOLCHAIN_CACHE","value":"/opt/falcone-toolchain-cache"},
        {"name":"FS_PROFILE_COMPONENTS","value":"/opt/falcone-toolchain-cache/profile"},
        {"name":"TMPDIR","value":"/tmp"}
    ]);
    Ok(pod)
}

#[test]
#[ignore = "UNVERIFIED: FS_TEST_CLUSTER unset"]
fn cluster_profile_go() -> Result<(), String> {
    if std::env::var_os("FS_TEST_CLUSTER").is_none() {
        // Adapt the shared harness's explicit-request contract to this suite's visible skip.
        return live_qualification().or_else(|error| {
            if error
                == format!("UNVERIFIED: {CLUSTER_TEST} was requested but FS_TEST_CLUSTER is unset")
            {
                Ok(())
            } else {
                Err(error)
            }
        });
    }
    if std::env::var_os("FS_TOOLCHAIN_CACHE").is_none() {
        return Err("UNVERIFIED: FS_TOOLCHAIN_CACHE is required; build the profile components in the trusted compiler image first".into());
    }
    let Some((image, mode, configs)) = validated_inputs(
        std::env::var("FS_TEST_CLUSTER").ok().as_deref(),
        std::env::var("KUBECONFIG").ok().as_deref(),
        std::env::var("FS_TEST_WAS02_IMAGE").ok().as_deref(),
        std::env::var("FS_TEST_NAMESPACE_MODE").ok().as_deref(),
        std::env::var("FS_TEST_NAMESPACE").ok().as_deref(),
    )?
    .map(|(image, mode, configs)| (image.to_owned(), mode, configs)) else {
        return Err("cluster gate missing".into());
    };
    let plan = NamespacePlan::new(QUALIFICATION_PREFIX, &mode)?;
    let pod = profile_pod(&plan, &image)?;
    let cluster = RealCluster::connect(&configs)?;
    let guard = plan.acquire(&cluster)?;
    let result = cluster.run_pod(guard.namespace(), &pod);
    guard.close()?;
    result
}

#[test]
fn profile_pod_runs_only_prebuilt_fixtures_with_the_restricted_posture() {
    let plan = NamespacePlan::new(QUALIFICATION_PREFIX, &NamespaceMode::Create).unwrap();
    let image = format!("registry.invalid/profile@sha256:{}", "a".repeat(64));
    let pod = profile_pod(&plan, &image).unwrap();
    let deployment = serde_json::json!({"apiVersion":"apps/v1","kind":"Deployment","spec":{"template":{"spec":pod["spec"]}}});
    assert!(
        falcone_serverless_worker::check(&deployment, falcone_serverless_worker::Role::GuestWorker)
            .is_empty()
    );
    assert!(plan.namespace().name().starts_with("fs-test-"));
    // The restricted posture the profile qualification must run under, stated explicitly.
    assert_eq!(pod["spec"]["securityContext"]["runAsNonRoot"], true);
    assert!(
        pod["spec"]["securityContext"]["runAsUser"]
            .as_u64()
            .unwrap()
            >= 100_000
    );
    assert_eq!(
        pod["spec"]["securityContext"]["seccompProfile"]["type"],
        "RuntimeDefault"
    );
    assert_eq!(
        pod["spec"]["containers"][0]["securityContext"]["readOnlyRootFilesystem"],
        true
    );
    assert_eq!(
        pod["spec"]["containers"][0]["securityContext"]["allowPrivilegeEscalation"],
        false
    );
    assert_eq!(pod["spec"]["automountServiceAccountToken"], false);
    assert_eq!(pod["metadata"]["labels"][RUN_LABEL], plan.run());
    assert_eq!(
        pod["spec"]["containers"][0]["command"][0],
        "/usr/local/bin/profile-go-runtime"
    );
    assert_eq!(
        pod["spec"]["containers"][0]["command"][2],
        "built_profile_components"
    );
}
