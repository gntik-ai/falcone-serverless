use namespace::{NamespaceMode, NamespacePlan, RUN_LABEL};

trait Cluster: namespace::NamespaceApi {
    fn run_pod(
        &self,
        namespace: &namespace::Namespace,
        pod: &serde_json::Value,
    ) -> Result<(), String>;
}

fn run(cluster: &impl Cluster, image: &str, mode: &NamespaceMode) -> Result<(), String> {
    let plan = NamespacePlan::new(QUALIFICATION_PREFIX, mode)?;
    let pod = pod_manifest(&plan, image)?;
    let guard = plan.acquire(cluster)?;
    let result = cluster.run_pod(guard.namespace(), &pod);
    let cleanup = guard.close();
    cleanup?;
    result
}

/// Validates the opt-in inputs without constructing a client. `Ok(None)` means the gate is unset.
fn validated_inputs<'a>(
    enabled: Option<&str>,
    config: Option<&str>,
    image: Option<&'a str>,
    mode: Option<&str>,
    namespace: Option<&str>,
) -> Result<Option<(&'a str, NamespaceMode, kubeconfig::Kubeconfigs)>, String> {
    match enabled {
        None => return Ok(None),
        Some("1") => {}
        Some(_) => return Err("FS_TEST_CLUSTER must be 1 when set".into()),
    }
    let image = image
        .filter(|image| image::pinned_image(image))
        .ok_or("UNVERIFIED: FS_TEST_WAS02_IMAGE must be <repo>@sha256:<digest>")?;
    let mode = NamespaceMode::parse(mode, namespace)?;
    let configs = kubeconfig::Kubeconfigs::parse(config)?;
    Ok(Some((image, mode, configs)))
}

fn opt_in<C: Cluster>(
    enabled: Option<&str>,
    config: Option<&str>,
    image: Option<&str>,
    mode: Option<&str>,
    namespace: Option<&str>,
    connect: impl FnOnce(&kubeconfig::Kubeconfigs) -> Result<C, String>,
) -> Result<bool, String> {
    let Some((image, mode, configs)) = validated_inputs(enabled, config, image, mode, namespace)?
    else {
        println!("UNVERIFIED: FS_TEST_CLUSTER unset; no client call");
        return Ok(false);
    };
    let cluster = connect(&configs)?;
    run(&cluster, image, &mode)?;
    Ok(true)
}

fn pod_manifest(plan: &NamespacePlan, image: &str) -> Result<serde_json::Value, String> {
    if !image::pinned_image(image) {
        return Err("UNVERIFIED: FS_TEST_WAS02_IMAGE must be <repo>@sha256:<digest>".into());
    }
    let uid = 100_000 + u64::from(rand::random::<u32>() % 1_000_000);
    Ok(serde_json::json!({
        "apiVersion":"v1", "kind":"Pod",
        "metadata":{"namespace":plan.namespace().name(), "name":format!("{QUALIFICATION_PREFIX}-{}", plan.run()), "labels":{RUN_LABEL:plan.run()}},
        "spec": {
            "restartPolicy":"Never", "activeDeadlineSeconds":1500,
            "automountServiceAccountToken":false,
            "securityContext":{"runAsNonRoot":true,"runAsUser":uid,"runAsGroup":uid,"fsGroup":uid,"seccompProfile":{"type":"RuntimeDefault"}},
            "containers":[{
                "name":"runner", "image":image,
                "command":["/usr/local/bin/compile-cache-qualification", QUALIFICATION_FLAG],
                "securityContext":{"allowPrivilegeEscalation":false,"readOnlyRootFilesystem":true,"capabilities":{"drop":["ALL"]}},
                "resources":{"requests":{"cpu":"1","memory":"512Mi"},"limits":{"cpu":"2","memory":"5Gi"}},
                "volumeMounts":[{"name":"scratch","mountPath":"/tmp"}]
            }],
            "volumes":[{"name":"scratch","emptyDir":{"sizeLimit":"256Mi"}}]
        }
    }))
}

#[test]
fn owned_manifest_is_restricted_and_uses_only_its_run_namespace() {
    let mode = NamespaceMode::Create;
    let plan = NamespacePlan::new("was02", &mode).unwrap();
    let image = format!("registry.invalid/compiler@sha256:{}", "a".repeat(64));
    let pod = pod_manifest(&plan, &image).unwrap();
    assert!(plan.namespace().name().starts_with("fs-test-"));
    assert_eq!(pod["metadata"]["namespace"], plan.namespace().name());
    assert_eq!(pod["metadata"]["labels"][RUN_LABEL], plan.run());
    assert!(
        pod["metadata"]["name"]
            .as_str()
            .unwrap()
            .ends_with(plan.run())
    );
    let spec = &pod["spec"];
    assert_eq!(spec["securityContext"]["runAsNonRoot"], true);
    assert_eq!(
        spec["securityContext"]["seccompProfile"]["type"],
        "RuntimeDefault"
    );
    assert!(spec["securityContext"]["runAsUser"].as_u64().unwrap() > 65532);
    assert_eq!(spec["automountServiceAccountToken"], false);
    let container = &spec["containers"][0];
    assert_eq!(container["securityContext"]["readOnlyRootFilesystem"], true);
    assert_eq!(
        container["securityContext"]["allowPrivilegeEscalation"],
        false
    );
    assert_eq!(
        container["securityContext"]["capabilities"]["drop"],
        serde_json::json!(["ALL"])
    );
    assert_eq!(container["image"], image);
    assert!(spec["volumes"][0]["emptyDir"].is_object());
    assert!(pod_manifest(&plan, "compiler:latest").is_err());
}

#[derive(Default)]
struct ClusterFixture {
    calls: std::cell::RefCell<Vec<String>>,
    deny: bool,
    pod_fails: bool,
}

impl namespace::NamespaceApi for ClusterFixture {
    fn can_create_namespaces(&self) -> Result<bool, String> {
        Ok(!self.deny)
    }
    fn create_namespace(&self, ns: &namespace::Namespace) -> Result<(), String> {
        assert_eq!(
            ns.manifest()["metadata"]["labels"]["pod-security.kubernetes.io/enforce"],
            "restricted"
        );
        self.calls
            .borrow_mut()
            .push(format!("create {}", ns.name()));
        Ok(())
    }
    fn namespace_labels(
        &self,
        _: &namespace::Namespace,
    ) -> Result<Option<std::collections::BTreeMap<String, String>>, String> {
        Ok(Some(
            [(
                "pod-security.kubernetes.io/enforce".into(),
                "restricted".into(),
            )]
            .into(),
        ))
    }
    fn delete_namespace(&self, ns: &namespace::Namespace) -> Result<(), String> {
        self.calls
            .borrow_mut()
            .push(format!("delete {}", ns.name()));
        Ok(())
    }
    fn delete_run_objects(&self, ns: &namespace::Namespace, run: &str) -> Result<(), String> {
        self.calls
            .borrow_mut()
            .push(format!("cleanup {} {run}", ns.name()));
        Ok(())
    }
}

impl Cluster for ClusterFixture {
    fn run_pod(&self, ns: &namespace::Namespace, pod: &serde_json::Value) -> Result<(), String> {
        assert_eq!(pod["metadata"]["namespace"], ns.name());
        let run = pod["metadata"]["labels"][RUN_LABEL].as_str().unwrap();
        assert!(pod["metadata"]["name"].as_str().unwrap().ends_with(run));
        self.calls
            .borrow_mut()
            .push(format!("pod {} {run}", ns.name()));
        if self.pod_fails {
            Err("fixture failure".into())
        } else {
            Ok(())
        }
    }
}

#[test]
fn namespace_modes_cleanup_only_owned_resources_even_on_failure() {
    let image = format!("registry.invalid/compiler@sha256:{}", "a".repeat(64));
    for fails in [false, true] {
        let cluster = ClusterFixture {
            pod_fails: fails,
            ..Default::default()
        };
        let result = run(&cluster, &image, &NamespaceMode::Create);
        assert_eq!(result.is_err(), fails);
        let calls = cluster.calls.borrow();
        assert_eq!(calls.len(), 3);
        let ns = calls[0].strip_prefix("create ").unwrap();
        assert!(calls[1].starts_with(&format!("pod {ns} ")));
        assert_eq!(calls[2], format!("delete {ns}"));

        let cluster = ClusterFixture {
            pod_fails: fails,
            ..Default::default()
        };
        let mode = NamespaceMode::Shared(namespace::Namespace::new("fs-test-shared").unwrap());
        assert_eq!(run(&cluster, &image, &mode).is_err(), fails);
        let calls = cluster.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1], calls[0].replacen("pod ", "cleanup ", 1));
    }
    let cluster = ClusterFixture {
        deny: true,
        ..Default::default()
    };
    assert!(
        run(&cluster, &image, &NamespaceMode::Create)
            .unwrap_err()
            .contains("FS_TEST_NAMESPACE_MODE")
    );
    assert!(cluster.calls.borrow().is_empty());
}

#[test]
fn unset_gate_prints_skip_without_constructing_a_client() {
    assert!(
        !opt_in(
            None,
            Some(":"),
            None,
            None,
            None,
            |_| -> Result<ClusterFixture, String> { panic!("client must not be constructed") }
        )
        .unwrap()
    );
    for image in [None, Some("registry.invalid/compiler:latest")] {
        assert!(
            opt_in(
                Some("1"),
                Some(":"),
                image,
                None,
                None,
                |_| -> Result<ClusterFixture, String> { panic!("client must not be constructed") }
            )
            .unwrap_err()
            .starts_with("UNVERIFIED")
        );
    }
}

struct RealCluster {
    runtime: tokio::runtime::Runtime,
    client: kube::Client,
}

fn api_error(_: kube::Error) -> String {
    "UNVERIFIED: cluster operation denied or unavailable".into()
}

impl RealCluster {
    fn connect(configs: &kubeconfig::Kubeconfigs) -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "UNVERIFIED: runtime unavailable")?;
        let config = runtime
            .block_on(kube::Config::from_custom_kubeconfig(
                configs.merged(),
                &Default::default(),
            ))
            .map_err(|_| "UNVERIFIED: unusable KUBECONFIG")?;
        let client = {
            let _guard = runtime.enter();
            kube::Client::try_from(config).map_err(|_| "UNVERIFIED: client unavailable")?
        };
        Ok(Self { runtime, client })
    }

    fn pods(&self, namespace: &namespace::Namespace) -> kube::Api<k8s_openapi::api::core::v1::Pod> {
        kube::Api::namespaced(self.client.clone(), namespace.name())
    }
}

impl namespace::NamespaceApi for RealCluster {
    fn can_create_namespaces(&self) -> Result<bool, String> {
        use k8s_openapi::api::authorization::v1::{
            ResourceAttributes, SelfSubjectAccessReview, SelfSubjectAccessReviewSpec,
        };
        let api: kube::Api<SelfSubjectAccessReview> = kube::Api::all(self.client.clone());
        let review = SelfSubjectAccessReview {
            spec: SelfSubjectAccessReviewSpec {
                resource_attributes: Some(ResourceAttributes {
                    resource: Some("namespaces".into()),
                    verb: Some("create".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        self.runtime
            .block_on(api.create(&Default::default(), &review))
            .map(|review| review.status.is_some_and(|status| status.allowed))
            .map_err(api_error)
    }

    fn create_namespace(&self, ns: &namespace::Namespace) -> Result<(), String> {
        let api: kube::Api<k8s_openapi::api::core::v1::Namespace> =
            kube::Api::all(self.client.clone());
        let object =
            serde_json::from_value(ns.manifest()).map_err(|_| "invalid namespace manifest")?;
        self.runtime
            .block_on(api.create(&Default::default(), &object))
            .map(drop)
            .map_err(api_error)
    }

    fn namespace_labels(
        &self,
        ns: &namespace::Namespace,
    ) -> Result<Option<std::collections::BTreeMap<String, String>>, String> {
        let api: kube::Api<k8s_openapi::api::core::v1::Namespace> =
            kube::Api::all(self.client.clone());
        self.runtime
            .block_on(api.get_opt(ns.name()))
            .map(|found| found.map(|ns| ns.metadata.labels.unwrap_or_default()))
            .map_err(api_error)
    }

    fn delete_namespace(&self, ns: &namespace::Namespace) -> Result<(), String> {
        let api: kube::Api<k8s_openapi::api::core::v1::Namespace> =
            kube::Api::all(self.client.clone());
        self.runtime
            .block_on(api.delete(ns.name(), &Default::default()))
            .map(drop)
            .map_err(api_error)
    }

    fn delete_run_objects(&self, ns: &namespace::Namespace, run: &str) -> Result<(), String> {
        let selector = kube::api::ListParams::default().labels(&format!("{RUN_LABEL}={run}"));
        self.runtime
            .block_on(
                self.pods(ns)
                    .delete_collection(&Default::default(), &selector),
            )
            .map(drop)
            .map_err(api_error)
    }
}

impl Cluster for RealCluster {
    fn run_pod(&self, ns: &namespace::Namespace, pod: &serde_json::Value) -> Result<(), String> {
        use kube::api::{LogParams, PostParams};
        let pods = self.pods(ns);
        let object = serde_json::from_value(pod.clone()).map_err(|_| "invalid pod manifest")?;
        // Create only; a name collision never authorizes update, patch or deletion.
        self.runtime
            .block_on(pods.create(&PostParams::default(), &object))
            .map_err(api_error)?;
        let name = pod["metadata"]["name"].as_str().ok_or("pod name missing")?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1500);
        loop {
            let observed = self.runtime.block_on(pods.get(name)).map_err(api_error)?;
            let phase = observed
                .status
                .and_then(|status| status.phase)
                .unwrap_or_default();
            if phase == "Failed" || phase == "Succeeded" {
                // Retain the sanitized records before the caller cleans the namespace up, on
                // success and on failure alike. A failed pod's logs are best effort.
                let logs = self
                    .runtime
                    .block_on(pods.logs(name, &LogParams::default()))
                    .map_err(api_error);
                let retained = match &logs {
                    Ok(logs) => retain_records(name, logs),
                    Err(error) => Err(error.clone()),
                };
                if phase == "Failed" {
                    return Err(match retained {
                        Ok(digest) => format!("WAS-02 qualification pod failed; {digest}"),
                        Err(_) => "WAS-02 qualification pod failed".into(),
                    });
                }
                let logs = logs?;
                retained?;
                return if logs.lines().any(|line| {
                    serde_json::from_str::<serde_json::Value>(line).is_ok_and(|record| {
                        record == serde_json::json!({"check":QUALIFICATION_CHECK,"status":"passed"})
                    })
                }) {
                    Ok(())
                } else {
                    Err("WAS-02 qualification success record missing".into())
                };
            }
            if std::time::Instant::now() >= deadline {
                return Err("WAS-02 qualification observation deadline".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }
}

/// Keeps only the structured qualification records of a pod log: JSON objects with a string
/// `check` field. Free-form lines (which could carry anything a process printed) are dropped.
fn sanitized_records(logs: &str) -> String {
    let mut kept = String::new();
    for line in logs.lines() {
        let record = serde_json::from_str::<serde_json::Value>(line).ok();
        if record
            .as_ref()
            .is_some_and(|record| record["check"].is_string() && record.is_object())
        {
            kept.push_str(line.trim());
            kept.push('\n');
        }
    }
    kept
}

fn records_digest(records: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(records.as_bytes()))
}

/// Persists the sanitized records of one pod as `<pod>.jsonl` with its `.sha256` next to it, in
/// `FS_TEST_EVIDENCE_DIR` (default: the cargo test scratch directory), and reports the path and
/// digest on stdout so the operator can reference the evidence by artifact and SHA256.
fn retain_records(pod_name: &str, logs: &str) -> Result<String, String> {
    let dir = std::env::var_os("FS_TEST_EVIDENCE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")));
    let summary = write_records(&dir, pod_name, logs)?;
    println!("EVIDENCE: {summary}");
    Ok(summary)
}

fn write_records(dir: &std::path::Path, pod_name: &str, logs: &str) -> Result<String, String> {
    let records = sanitized_records(logs);
    let digest = records_digest(&records);
    let failed = |_| "qualification records not retained".to_string();
    std::fs::create_dir_all(dir).map_err(failed)?;
    let file = dir.join(format!("{pod_name}.jsonl"));
    std::fs::write(&file, &records).map_err(failed)?;
    std::fs::write(
        dir.join(format!("{pod_name}.jsonl.sha256")),
        format!("{digest}  {pod_name}.jsonl\n"),
    )
    .map_err(failed)?;
    Ok(format!("records {} sha256 {digest}", file.display()))
}

#[test]
fn retained_records_keep_only_structured_checks_and_carry_a_digest() {
    let logs = "starting\n{\"check\":\"execution_api\",\"case\":\"identity\",\"status\":\"passed\"}\n\
                {\"unrelated\":1}\nsecret-looking free text\n[1,2]\n\
                {\"check\":\"execution_api\",\"status\":\"passed\"}\n";
    let kept = sanitized_records(logs);
    assert_eq!(
        kept,
        "{\"check\":\"execution_api\",\"case\":\"identity\",\"status\":\"passed\"}\n\
         {\"check\":\"execution_api\",\"status\":\"passed\"}\n"
    );
    // SHA-256 of the empty input, a known literal.
    assert_eq!(
        records_digest(""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(sanitized_records("no records here\n"), "");
}

#[test]
fn records_are_written_with_a_digest_file_that_matches_them() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("retain-records-test");
    let _ = std::fs::remove_dir_all(&dir);
    let logs = "noise\n{\"check\":\"execution_api\",\"case\":\"saturation\",\"status\":\"failed\"}\n";
    let summary = write_records(&dir, "run01-abc", logs).unwrap();
    let written = std::fs::read_to_string(dir.join("run01-abc.jsonl")).unwrap();
    assert_eq!(
        written,
        "{\"check\":\"execution_api\",\"case\":\"saturation\",\"status\":\"failed\"}\n"
    );
    let sidecar = std::fs::read_to_string(dir.join("run01-abc.jsonl.sha256")).unwrap();
    let digest = sidecar.split_whitespace().next().unwrap();
    assert_eq!(digest.len(), 64);
    assert!(summary.ends_with(digest));
    assert_eq!(digest, records_digest(&written));
}

#[test]
fn live_qualification_reports_ignored_when_cluster_unset() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CLUSTER_TEST])
        .env_remove("FS_TEST_CLUSTER")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains(&format!(
            "test {CLUSTER_TEST} ... ignored, UNVERIFIED: FS_TEST_CLUSTER unset"
        )),
        "live qualification must visibly report its skip reason: {stdout}"
    );
    assert!(stdout.contains("0 passed; 0 failed; 1 ignored"), "{stdout}");
}

fn live_qualification() -> Result<(), String> {
    // Only runs when requested (--ignored or --include-ignored); an unset gate is then a
    // failure, never a pass with no qualification executed.
    let ran = opt_in(
        std::env::var("FS_TEST_CLUSTER").ok().as_deref(),
        std::env::var("KUBECONFIG").ok().as_deref(),
        std::env::var("FS_TEST_WAS02_IMAGE").ok().as_deref(),
        std::env::var("FS_TEST_NAMESPACE_MODE").ok().as_deref(),
        std::env::var("FS_TEST_NAMESPACE").ok().as_deref(),
        RealCluster::connect,
    )?;
    if ran {
        Ok(())
    } else {
        Err(format!(
            "UNVERIFIED: {CLUSTER_TEST} was requested but FS_TEST_CLUSTER is unset"
        ))
    }
}

/// Runs in every `cargo test`. With FS_TEST_CLUSTER set, invalid opt-in inputs fail here without
/// constructing a client, and a valid opt-in fails unless the ignored live qualification was
/// requested, so setting the gate can never yield a green run with no qualification executed.
#[test]
fn cluster_gate_inputs_fail_closed_when_set() -> Result<(), String> {
    let (enabled, config, image, mode, namespace) = (
        std::env::var("FS_TEST_CLUSTER").ok(),
        std::env::var("KUBECONFIG").ok(),
        std::env::var("FS_TEST_WAS02_IMAGE").ok(),
        std::env::var("FS_TEST_NAMESPACE_MODE").ok(),
        std::env::var("FS_TEST_NAMESPACE").ok(),
    );
    let inputs = validated_inputs(
        enabled.as_deref(),
        config.as_deref(),
        image.as_deref(),
        mode.as_deref(),
        namespace.as_deref(),
    )?;
    let live_requested =
        std::env::args().any(|arg| arg == "--ignored" || arg == "--include-ignored");
    if inputs.is_some() && !live_requested {
        return Err(
            "UNVERIFIED: FS_TEST_CLUSTER=1 runs the live qualification only with --include-ignored"
                .into(),
        );
    }
    Ok(())
}

fn run_self(args: &[&str], env: &[(&str, &str)]) -> (bool, String) {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(args)
        .env_remove("FS_TEST_CLUSTER")
        .env_remove("FS_TEST_WAS02_IMAGE")
        .env_remove("KUBECONFIG");
    for (name, value) in env {
        command.env(name, value);
    }
    let output = command.output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    (output.status.success(), text)
}

#[test]
fn gate_set_with_invalid_inputs_fails_the_default_run() {
    let (passed, output) = run_self(
        &["--exact", "cluster_gate_inputs_fail_closed_when_set"],
        &[
            ("FS_TEST_CLUSTER", "1"),
            ("FS_TEST_WAS02_IMAGE", "not-pinned"),
        ],
    );
    assert!(
        !passed,
        "an invalid opt-in must fail the default run: {output}"
    );
    assert!(
        output.contains("UNVERIFIED: FS_TEST_WAS02_IMAGE must be <repo>@sha256:<digest>"),
        "{output}"
    );
}

#[test]
fn explicit_live_request_without_gate_reports_the_suite_contract() {
    let (passed, output) = run_self(&["--exact", CLUSTER_TEST, "--ignored", "--nocapture"], &[]);
    if SKIP_EXPLICIT_REQUEST_WITHOUT_GATE {
        // WAS-01 requires an explicit skip even when requested with --ignored.
        assert!(passed, "an unset guest-execute gate must skip: {output}");
        assert!(
            output.contains("UNVERIFIED: FS_TEST_CLUSTER unset; no client call"),
            "{output}"
        );
    } else {
        assert!(!passed, "an unset gate must not read as a pass: {output}");
        assert!(output.contains("FS_TEST_CLUSTER is unset"), "{output}");
    }
}

#[test]
fn unset_gate_is_skipped_and_invalid_opt_in_fails_closed() {
    assert!(
        !opt_in(
            None,
            Some(":"),
            None,
            None,
            None,
            |_| -> Result<ClusterFixture, String> { panic!("client must not be constructed") }
        )
        .unwrap()
    );
    for gate in ["0", "true", ""] {
        assert!(
            opt_in(
                Some(gate),
                None,
                None,
                None,
                None,
                |_| -> Result<ClusterFixture, String> { panic!("client must not be constructed") }
            )
            .is_err()
        );
    }
    let pinned = format!("registry.invalid/worker@sha256:{}", "a".repeat(64));
    for config in [None, Some(":"), Some("/nonexistent-was02-kubeconfig")] {
        assert!(
            opt_in(
                Some("1"),
                config,
                Some(&pinned),
                None,
                None,
                |_| -> Result<ClusterFixture, String> {
                    panic!("invalid inputs reached client construction")
                }
            )
            .is_err()
        );
    }
    for (mode, namespace) in [
        ("invalid", None),
        ("shared", None),
        ("shared", Some("off-limits")),
    ] {
        assert!(
            opt_in(
                Some("1"),
                None,
                Some(&pinned),
                Some(mode),
                namespace,
                |_| -> Result<ClusterFixture, String> {
                    panic!("invalid namespace reached client construction")
                }
            )
            .is_err()
        );
    }
}
