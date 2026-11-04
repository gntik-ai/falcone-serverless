//! Operator evidence only: FS_TEST_CLUSTER=1 and FS_TEST_WAS02_IMAGE=<repo>@sha256:<digest>.
//! The image is built and pinned by the operator; evidence belongs on #52.
#![cfg(target_os = "linux")]

#[path = "support/image.rs"]
pub mod image;
#[path = "support/kubeconfig.rs"]
pub mod kubeconfig;
#[path = "support/namespace.rs"]
pub mod namespace;

use namespace::{NamespaceMode, NamespacePlan, RUN_LABEL};

trait Cluster: namespace::NamespaceApi {
    fn run_pod(
        &self,
        namespace: &namespace::Namespace,
        pod: &serde_json::Value,
    ) -> Result<(), String>;
}

fn run(cluster: &impl Cluster, image: &str, mode: &NamespaceMode) -> Result<(), String> {
    let plan = NamespacePlan::new("was02", mode)?;
    let pod = pod_manifest(&plan, image)?;
    let guard = plan.acquire(cluster)?;
    let result = cluster.run_pod(guard.namespace(), &pod);
    let cleanup = guard.close();
    cleanup?;
    result
}

fn opt_in<C: Cluster>(
    enabled: Option<&str>,
    config: Option<&str>,
    image: Option<&str>,
    mode: Option<&str>,
    namespace: Option<&str>,
    connect: impl FnOnce(&kubeconfig::Kubeconfigs) -> Result<C, String>,
) -> Result<bool, String> {
    if enabled != Some("1") {
        println!("UNVERIFIED: FS_TEST_CLUSTER is unset; no client call");
        return Ok(false);
    }
    let image = image
        .filter(|image| image::pinned_image(image))
        .ok_or("UNVERIFIED: FS_TEST_WAS02_IMAGE must be <repo>@sha256:<digest>")?;
    let mode = NamespaceMode::parse(mode, namespace)?;
    let configs = kubeconfig::Kubeconfigs::parse(config)?;
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
        "metadata":{"namespace":plan.namespace().name(), "name":format!("compile-{}", plan.run()), "labels":{RUN_LABEL:plan.run()}},
        "spec": {
            "restartPolicy":"Never", "activeDeadlineSeconds":1500,
            "automountServiceAccountToken":false,
            "securityContext":{"runAsNonRoot":true,"runAsUser":uid,"runAsGroup":uid,"fsGroup":uid,"seccompProfile":{"type":"RuntimeDefault"}},
            "containers":[{
                "name":"runner", "image":image,
                "command":["/usr/local/bin/compile-cache-qualification"],
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
    assert_eq!(
        container["command"],
        serde_json::json!(["/usr/local/bin/compile-cache-qualification"])
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
fn unset_gate_prints_unverified_without_constructing_a_client() {
    for gate in [None, Some("0"), Some("true")] {
        assert!(
            !opt_in(
                gate,
                Some(":"),
                None,
                None,
                None,
                |_| -> Result<ClusterFixture, String> { panic!("client must not be constructed") }
            )
            .unwrap()
        );
    }
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
            if phase == "Failed" {
                return Err("compiler qualification pod failed".into());
            }
            if phase == "Succeeded" {
                let logs = self
                    .runtime
                    .block_on(pods.logs(name, &LogParams::default()))
                    .map_err(api_error)?;
                return if logs.lines().any(|line| {
                    serde_json::from_str::<serde_json::Value>(line).is_ok_and(|record| {
                        record == serde_json::json!({"check":"compile_cache","status":"passed"})
                    })
                }) {
                    Ok(())
                } else {
                    Err("compiler qualification success record missing".into())
                };
            }
            if std::time::Instant::now() >= deadline {
                return Err("compiler qualification observation deadline".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }
}

#[test]
fn cluster_compile_cache() -> Result<(), String> {
    opt_in(
        std::env::var("FS_TEST_CLUSTER").ok().as_deref(),
        std::env::var("KUBECONFIG").ok().as_deref(),
        std::env::var("FS_TEST_WAS02_IMAGE").ok().as_deref(),
        std::env::var("FS_TEST_NAMESPACE_MODE").ok().as_deref(),
        std::env::var("FS_TEST_NAMESPACE").ok().as_deref(),
        RealCluster::connect,
    )
    .map(drop)
}
