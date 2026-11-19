//! Operator-only guest execution. Static rejection precedes any cluster client or build.
#![cfg(target_os = "linux")]
#[path = "support/image.rs"]
pub mod image;
#[path = "support/kubeconfig.rs"]
pub mod kubeconfig;
#[path = "support/namespace.rs"]
pub mod namespace;
#[path = "support/javascript_profile.rs"]
pub mod profile;

const QUALIFICATION_PREFIX: &str = "profile-js";
const QUALIFICATION_FLAG: &str = "--profile-javascript";
const QUALIFICATION_CHECK: &str = "profile_javascript";
const CLUSTER_TEST: &str = "cluster_profile_javascript";
const SKIP_EXPLICIT_REQUEST_WITHOUT_GATE: bool = true;
const PRODUCTION_IMAGE_ENV: Option<&str> = None;

include!("support/was02_cluster.rs");

fn profile_pod(plan: &NamespacePlan, image: &str) -> Result<serde_json::Value, String> {
    let mut pod = base_pod_manifest(plan, image)?;
    pod["spec"]["containers"][0]["command"] = serde_json::json!([
        "/usr/local/bin/profile-javascript",
        "--exact",
        "built_reference_envelopes",
        "--ignored",
        "--nocapture"
    ]);
    pod["spec"]["containers"][0]["env"] = serde_json::json!([
        {"name":"FS_JS_PROFILE_ROOT","value":"/opt/falcone/worker/qualification/javascript"},
        {"name":"FS_TEST_CLUSTER","value":"1"},
        {"name":"TMPDIR","value":"/tmp"}
    ]);
    Ok(pod)
}

fn static_gate() -> Result<(), String> {
    let checks = profile::static_checks(&profile::manifest());
    if checks.values().all(|passed| *passed) {
        Ok(())
    } else {
        Err("blocked: JavaScript static qualification failed; no cluster client, component build or guest instance permitted".into())
    }
}

#[test]
#[ignore = "UNVERIFIED: FS_TEST_CLUSTER unset"]
fn cluster_profile_javascript() -> Result<(), String> {
    if std::env::var_os("FS_TEST_CLUSTER").is_none() {
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
    static_gate()?;
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
fn blocked_reference_refuses_before_cluster_access() {
    assert!(static_gate().unwrap_err().contains("no cluster client"));
}

#[test]
fn envelope_comparison_rejects_each_response_and_dependency_mismatch() {
    let manifest = profile::manifest();
    for case in manifest["envelopes"].as_array().unwrap() {
        let fixture: serde_json::Value = serde_json::from_slice(
            &std::fs::read(profile::profile_dir().join(case["fixture"].as_str().unwrap())).unwrap(),
        )
        .unwrap();
        assert!(profile::envelope_matches(
            case,
            &fixture["expected"],
            &case["expectedCalls"]
        ));
        for field in ["statusCode", "headers", "body"] {
            let mut changed = fixture["expected"].clone();
            changed[field] = serde_json::json!("mismatch");
            assert!(!profile::envelope_matches(
                case,
                &changed,
                &case["expectedCalls"]
            ));
        }
        assert!(!profile::envelope_matches(
            case,
            &fixture["expected"],
            &serde_json::json!(["unexpected.call"])
        ));
    }
}

#[test]
#[ignore = "UNVERIFIED: requires static pass and trusted-image-built reference component in the cluster pod"]
fn built_reference_envelopes() -> Result<(), String> {
    use falcone_serverless_worker::{api, artifact::supervisor::CompileLimits, cache, host};
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex, atomic::AtomicBool};
    use std::time::{Duration, Instant};
    if std::env::var("FS_TEST_CLUSTER").as_deref() != Ok("1") {
        return Err("UNVERIFIED: reference guest execution requires FS_TEST_CLUSTER=1".into());
    }
    static_gate()?;
    struct Platform {
        state: Value,
        calls: Mutex<Vec<String>>,
    }
    impl host::Bindings for Platform {
        fn call(
            &self,
            _: &host::Context,
            operation: host::Operation,
        ) -> Result<host::Reply, host::HostError> {
            match operation {
                host::Operation::Read { .. } => {
                    self.calls.lock().unwrap().push("storage.read".into());
                    Ok(host::Reply::Data(
                        serde_json::to_vec(&self.state["currentSpec"]).unwrap(),
                    ))
                }
                host::Operation::Publish { .. } => {
                    self.calls.lock().unwrap().push("events.publish".into());
                    Ok(host::Reply::Empty)
                }
                _ => Err(host::HostError::Denied),
            }
        }
    }
    let manifest = profile::manifest();
    let bytes = std::fs::read(
        profile::profile_dir().join(
            manifest["component"]["path"]
                .as_str()
                .ok_or("component missing")?,
        ),
    )
    .map_err(|_| "component missing")?;
    let scratch = std::env::temp_dir().join(format!("profile-js-{}", std::process::id()));
    std::fs::create_dir(&scratch).map_err(|_| "scratch unavailable")?;
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let scratch = Scratch(scratch);
    let executable = std::env::current_exe().map_err(|_| "runner unavailable")?;
    let child = executable.parent().unwrap().join("artifact-compile-child");
    let mut compiler = cache::Cache::new(&scratch.0, child).map_err(|_| "compiler unavailable")?;
    let loaded = compiler
        .compile(
            cache::CONTRACT_VERSION,
            &bytes,
            CompileLimits {
                input_bytes: 64 * 1024 * 1024,
                cpu_seconds: 120,
                memory_bytes: 4 * 1024 * 1024 * 1024,
                wall_clock_deadline: Instant::now() + Duration::from_secs(300),
            },
            &AtomicBool::new(false),
        )
        .map_err(|_| "blocked: trusted component compilation failed")?;
    let mut envelopes = Vec::new();
    for case in manifest["envelopes"].as_array().unwrap() {
        let fixture: Value = serde_json::from_slice(
            &std::fs::read(profile::profile_dir().join(case["fixture"].as_str().unwrap()))
                .map_err(|_| "fixture missing")?,
        )
        .map_err(|_| "invalid fixture")?;
        let request = api::parse_request(include_bytes!(
            "fixtures/contract/request/valid/wasm-no-base.json"
        ))
        .map_err(|_| "invalid request")?;
        let platform = Arc::new(Platform {
            state: fixture["dependencyState"].clone(),
            calls: Mutex::new(vec![]),
        });
        let input = falcone_serverless_worker::adapters::javascript::Input {
            params: serde_json::to_vec(&fixture["params"]).map_err(|_| "invalid params")?,
            context: request.plan().context,
        }
        .wire()
        .map_err(|_| "invalid packet")?;
        let execution = host::execute(
            &loaded,
            host::Host::new(request.plan().context, platform.clone()),
            &input,
        );
        let output = execution
            .result
            .map_err(|_| "blocked: reference execution failed")?;
        let envelope: Value =
            serde_json::from_slice(&output).map_err(|_| "blocked: invalid reference envelope")?;
        envelopes.push(profile::envelope_matches(
            case,
            &envelope,
            &json!(*platform.calls.lock().unwrap()),
        ));
    }
    if profile::verdict(&profile::static_checks(&manifest), &envelopes) != "qualified" {
        return Err("blocked: reference envelope or storage/events calls mismatch".into());
    }
    println!("{}", json!({"check":QUALIFICATION_CHECK,"status":"passed"}));
    Ok(())
}

#[test]
fn reference_profile_pod_is_restricted_and_run_scoped() {
    let plan = NamespacePlan::new(QUALIFICATION_PREFIX, &NamespaceMode::Create).unwrap();
    let image = format!("registry.invalid/qualification@sha256:{}", "a".repeat(64));
    let pod = profile_pod(&plan, &image).unwrap();
    assert!(plan.namespace().name().starts_with("fs-test-"));
    assert_eq!(pod["metadata"]["namespace"], plan.namespace().name());
    assert_eq!(pod["metadata"]["labels"][RUN_LABEL], plan.run());
    let spec = &pod["spec"];
    assert_eq!(
        spec["securityContext"]["seccompProfile"]["type"],
        "RuntimeDefault"
    );
    assert!(spec["securityContext"]["runAsUser"].as_u64().unwrap() > 65532);
    let deployment = serde_json::json!({"apiVersion":"apps/v1","kind":"Deployment","spec":{"template":{"spec":spec}}});
    assert!(
        falcone_serverless_worker::check(&deployment, falcone_serverless_worker::Role::GuestWorker)
            .is_empty()
    );
    assert_eq!(
        spec["containers"][0]["command"][0],
        "/usr/local/bin/profile-javascript"
    );
    assert_eq!(
        spec["containers"][0]["command"][2],
        "built_reference_envelopes"
    );
    assert!(profile_pod(&plan, "qualification:latest").is_err());
}
