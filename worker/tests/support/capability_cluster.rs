//! Shared restricted-pod qualification transport, opt-in gates and fake cluster.
use super::{image, kubeconfig, namespace};
use namespace::{NamespaceMode, NamespacePlan, RUN_LABEL};

pub trait Cluster: namespace::NamespaceApi {
    fn run_pod(
        &self,
        namespace: &namespace::Namespace,
        pod: &serde_json::Value,
    ) -> Result<(), String>;
}

/// Validates the opt-in inputs without constructing a client. `Ok(None)` means the gate is unset.
pub fn validated_inputs<'a>(
    enabled: Option<&str>,
    config: Option<&str>,
    image: Option<&'a str>,
    mode: Option<&str>,
    namespace: Option<&str>,
) -> Result<Option<(&'a str, NamespaceMode, kubeconfig::Kubeconfigs)>, String> {
    match enabled {
        None => return Ok(None),
        Some("1") => {}
        Some(_) => return Err("UNVERIFIED: FS_TEST_CLUSTER must be 1 when set".into()),
    }
    let image = image
        .filter(|image| image::pinned_image(image))
        .ok_or("UNVERIFIED: FS_TEST_WAS02_IMAGE must be <repo>@sha256:<digest>")?;
    let unverified = |reason: String| {
        if reason.starts_with("UNVERIFIED:") {
            reason
        } else {
            format!("UNVERIFIED: {reason}")
        }
    };
    let mode = NamespaceMode::parse(mode, namespace).map_err(unverified)?;
    let configs = kubeconfig::Kubeconfigs::parse(config).map_err(unverified)?;
    Ok(Some((image, mode, configs)))
}

pub fn opt_in<C: Cluster>(
    enabled: Option<&str>,
    config: Option<&str>,
    image: Option<&str>,
    mode: Option<&str>,
    namespace: Option<&str>,
    connect: impl FnOnce(&kubeconfig::Kubeconfigs) -> Result<C, String>,
    run: impl FnOnce(&C, &str, &NamespaceMode) -> Result<(), String>,
) -> Result<bool, String> {
    let Some((image, mode, configs)) = validated_inputs(enabled, config, image, mode, namespace)?
    else {
        println!("not run: FS_TEST_CLUSTER unset");
        return Ok(false);
    };
    let cluster = connect(&configs)?;
    run(&cluster, image, &mode)?;
    Ok(true)
}

pub fn pod_manifest(
    plan: &NamespacePlan,
    image: &str,
    prefix: &str,
    mode: &str,
) -> Result<serde_json::Value, String> {
    if !image::pinned_image(image) {
        return Err("UNVERIFIED: FS_TEST_WAS02_IMAGE must be <repo>@sha256:<digest>".into());
    }
    let uid = 100_000 + u64::from(rand::random::<u32>() % 1_000_000);
    Ok(serde_json::json!({
        "apiVersion":"v1", "kind":"Pod",
        "metadata":{"namespace":plan.namespace().name(), "name":format!("{prefix}-{}", plan.run()), "labels":{RUN_LABEL:plan.run()}},
        "spec": {
            "restartPolicy":"Never", "activeDeadlineSeconds":1500,
            "automountServiceAccountToken":false,
            "securityContext":{"runAsNonRoot":true,"runAsUser":uid,"runAsGroup":uid,"fsGroup":uid,"seccompProfile":{"type":"RuntimeDefault"}},
            "containers":[{
                "name":"runner", "image":image,
                "command":["/usr/local/bin/compile-cache-qualification", mode],
                "securityContext":{"allowPrivilegeEscalation":false,"readOnlyRootFilesystem":true,"capabilities":{"drop":["ALL"]}},
                "resources":{"requests":{"cpu":"1","memory":"512Mi"},"limits":{"cpu":"2","memory":"5Gi"}},
                "volumeMounts":[{"name":"scratch","mountPath":"/tmp"}]
            }],
            "volumes":[{"name":"scratch","emptyDir":{"sizeLimit":"256Mi"}}]
        }
    }))
}

#[derive(Default)]
pub struct ClusterFixture {
    pub calls: std::cell::RefCell<Vec<String>>,
    pub deny: bool,
    pub pod_fails: bool,
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

pub struct RealCluster {
    runtime: tokio::runtime::Runtime,
    client: kube::Client,
    verify_records: fn(&str) -> Result<(), String>,
}

fn api_error(_: kube::Error) -> String {
    "UNVERIFIED: cluster operation denied or unavailable".into()
}

impl RealCluster {
    pub fn connect(
        configs: &kubeconfig::Kubeconfigs,
        verify_records: fn(&str) -> Result<(), String>,
    ) -> Result<Self, String> {
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
        Ok(Self {
            runtime,
            client,
            verify_records,
        })
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
                return Err("qualification pod failed".into());
            }
            if phase == "Succeeded" {
                let logs = self
                    .runtime
                    .block_on(pods.logs(name, &LogParams::default()))
                    .map_err(api_error)?;
                return (self.verify_records)(&logs);
            }
            if std::time::Instant::now() >= deadline {
                return Err("qualification observation deadline".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }
}
