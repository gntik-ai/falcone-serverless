//! The real cluster behind `Was01Cluster`, built on the shared `Cluster` core (see `cluster.rs`),
//! which uses only the explicitly listed kubeconfig files. `Config::infer`, the default
//! `from_kubeconfig`, the in-cluster configuration and `~/.kube/config` are never referenced.
//! Credentials are neither printed nor logged; errors carry the API status message only.

pub use super::cluster::load_config;
use super::cluster::{Cluster, failure, resource, review};
use super::observation::{Admission, Collector, Condition, ContainerView, Exit};
use super::was01::{Identity, Observed, Token, Was01Cluster};
use super::{Kubeconfigs, Namespace, NamespaceApi, RUN_LABEL};
use k8s_openapi::api::authentication::v1::{TokenRequest, TokenRequestSpec};
use k8s_openapi::api::core::v1::{Pod, ServiceAccount};
use kube::api::{ListParams, LogParams, PostParams};
use kube::config::AuthInfo;
use kube::core::DynamicObject;
use kube::{Api, Client, Config};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(200);
const OBSERVE: Duration = Duration::from_secs(180);
const RECORDS: usize = 9;
const CONTAINER: &str = "runner";

pub struct KubeCluster {
    cluster: Cluster,
    config: Config,
}

impl KubeCluster {
    pub fn connect(kubeconfigs: &Kubeconfigs) -> Result<Self, String> {
        let (cluster, config) = Cluster::connect(kubeconfigs)?;
        Ok(Self { cluster, config })
    }

    fn block<T>(&self, future: impl std::future::Future<Output = T>) -> T {
        self.cluster.block(future)
    }

    /// A client that carries only the installer token: none of the admin credentials.
    fn installer(&self, token: &Token) -> Result<Client, String> {
        let mut config = self.config.clone();
        config.auth_info = serde_json::from_value::<AuthInfo>(json!({"token": token.expose()}))
            .map_err(|_| "cannot build the installer identity".to_string())?;
        self.block(async { Client::try_from(config) })
            .map_err(|_| "cannot build the installer client".to_string())
    }

    fn dynamic(
        client: Client,
        namespace: &str,
        object: &Value,
    ) -> Result<Api<DynamicObject>, String> {
        let (group, version) = match object["apiVersion"].as_str() {
            Some(api) => api.rsplit_once('/').unwrap_or(("", api)),
            None => return Err("object has no apiVersion".into()),
        };
        let kind = object["kind"].as_str().ok_or("object has no kind")?;
        Ok(Api::namespaced_with(
            client,
            namespace,
            &resource(group, version, kind)?,
        ))
    }

    /// After cleanup: is the namespace gone, or at least Terminating?
    pub fn namespace_gone_or_terminating(&self, namespace: &Namespace) -> Result<bool, String> {
        self.cluster.namespace_gone_or_terminating(namespace)
    }
}

impl NamespaceApi for KubeCluster {
    fn can_create_namespaces(&self) -> Result<bool, String> {
        self.cluster.can_create_namespaces()
    }

    fn create_namespace(&self, namespace: &Namespace) -> Result<(), String> {
        self.cluster.create_namespace(namespace)
    }

    fn namespace_labels(
        &self,
        namespace: &Namespace,
    ) -> Result<Option<BTreeMap<String, String>>, String> {
        self.cluster.namespace_labels(namespace)
    }

    fn delete_namespace(&self, namespace: &Namespace) -> Result<(), String> {
        self.cluster.delete_namespace(namespace)
    }

    fn delete_run_objects(&self, namespace: &Namespace, run: &str) -> Result<(), String> {
        self.cluster.delete_run_objects(namespace, run)
    }
}

impl Was01Cluster for KubeCluster {
    fn apply(
        &self,
        identity: &Identity<'_>,
        namespace: &Namespace,
        object: &Value,
    ) -> Result<Admission, String> {
        let client = match identity {
            Identity::Admin => self.cluster.admin().clone(),
            Identity::Installer(token) => self.installer(token)?,
        };
        let api = Self::dynamic(client, namespace.name(), object)?;
        let data: DynamicObject = serde_json::from_value(object.clone())
            .map_err(|_| "object is not a Kubernetes object".to_string())?;
        let name = data.metadata.name.clone().unwrap_or_default();
        let kind = object["kind"].as_str().unwrap_or_default().to_owned();
        match self.block(api.create(&PostParams::default(), &data)) {
            Ok(created) => Ok(Admission {
                identity: identity.name(),
                kind,
                name: created.metadata.name.unwrap_or(name),
                admitted: true,
                message: "created".to_owned(),
            }),
            Err(error) => Err(format!(
                "{} {kind} {name} refused: {}",
                identity.name(),
                failure(&error)
            )),
        }
    }

    fn token_request(&self, namespace: &Namespace, service_account: &str) -> Result<Token, String> {
        let api: Api<ServiceAccount> =
            Api::namespaced(self.cluster.admin().clone(), namespace.name());
        let request = TokenRequest {
            spec: TokenRequestSpec {
                expiration_seconds: Some(900),
                ..TokenRequestSpec::default()
            },
            ..TokenRequest::default()
        };
        let issued = self
            .block(api.create_token_request(service_account, &PostParams::default(), &request))
            .map_err(|error| failure(&error))?;
        issued
            .status
            .map(|status| Token::new(status.token))
            .ok_or_else(|| "the API server returned no token".to_string())
    }

    fn review_create(&self, token: &Token, group: &str, resource: &str) -> Result<bool, String> {
        let client = self.installer(token)?;
        self.block(review(client, group, resource))
    }

    fn observe(&self, token: &Token, namespace: &Namespace, run: &str) -> Result<Observed, String> {
        let client = self.installer(token)?;
        let pods: Api<Pod> = Api::namespaced(client, namespace.name());
        let selector = ListParams::default().labels(&format!("{RUN_LABEL}={run}"));
        let deadline = Instant::now() + OBSERVE;
        let mut collector = Collector::default();
        loop {
            let listed = self
                .block(pods.list(&selector))
                .map_err(|error| failure(&error))?;
            if let Some(pod) = listed.items.first() {
                let (conditions, view) = pod_view(pod);
                let has_output = view.has_output();
                collector.pod(conditions, view);
                // Read the log whatever the readiness: a runner that fails while loading
                // exits before the pod is ever Ready, and its message is the evidence.
                if has_output && let Some(name) = pod.metadata.name.as_deref() {
                    for previous in [false, true] {
                        let params = LogParams {
                            container: Some(CONTAINER.to_owned()),
                            previous,
                            ..LogParams::default()
                        };
                        if let Ok(log) = self.block(pods.logs(name, &params)) {
                            collector.log(log);
                        }
                    }
                }
                if collector.finished(RECORDS) {
                    return Ok(collector.into_observed());
                }
            }
            if Instant::now() >= deadline {
                return Ok(collector.into_observed());
            }
            std::thread::sleep(POLL);
        }
    }
}

/// The pod's conditions and the runner container's state.
pub fn pod_view(pod: &Pod) -> (Vec<Condition>, ContainerView) {
    let status = pod.status.as_ref();
    let conditions = status
        .iter()
        .flat_map(|status| status.conditions.iter().flatten())
        .map(|condition| Condition {
            kind: condition.type_.clone(),
            status: condition.status.clone(),
        })
        .collect();
    let exit = |terminated: &k8s_openapi::api::core::v1::ContainerStateTerminated| Exit {
        reason: terminated
            .reason
            .clone()
            .unwrap_or_else(|| "Terminated".to_owned()),
        code: terminated.exit_code,
    };
    let mut view = ContainerView::default();
    let runner = status.and_then(|status| {
        status
            .container_statuses
            .iter()
            .flatten()
            .find(|c| c.name == CONTAINER)
    });
    if let Some(runner) = runner {
        if let Some(state) = &runner.state {
            view.running = state.running.is_some();
            view.terminated = state.terminated.as_ref().map(exit);
            view.waiting_reason = state.waiting.as_ref().map(|waiting| {
                waiting
                    .reason
                    .clone()
                    .unwrap_or_else(|| "Waiting".to_owned())
            });
        }
        view.last_exit = runner
            .last_state
            .as_ref()
            .and_then(|state| state.terminated.as_ref())
            .map(exit);
    }
    (conditions, view)
}
