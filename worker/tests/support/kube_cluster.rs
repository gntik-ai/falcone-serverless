//! The real cluster behind `Was01Cluster`, built on kube with only the explicitly listed
//! kubeconfig files: each is read with `Kubeconfig::read_from` (see `kubeconfig.rs`) and the
//! merged result goes to `Config::from_custom_kubeconfig`. `Config::infer`, the default `from_kubeconfig`, the
//! in-cluster configuration and `~/.kube/config` are never referenced. Credentials are
//! neither printed nor logged; errors carry the API status message only.

use super::observation::{Admission, Collector, Condition, ContainerView, Exit};
use super::was01::{Identity, Observed, Token, Was01Cluster};
use super::{Kubeconfigs, Namespace, NamespaceApi, RUN_LABEL};
use k8s_openapi::api::authentication::v1::{TokenRequest, TokenRequestSpec};
use k8s_openapi::api::authorization::v1::{
    ResourceAttributes, SelfSubjectAccessReview, SelfSubjectAccessReviewSpec,
};
use k8s_openapi::api::core::v1::{Namespace as KubeNamespace, Pod, ServiceAccount};
use kube::api::{DeleteParams, ListParams, LogParams, PostParams};
use kube::config::{AuthInfo, KubeConfigOptions};
use kube::core::{ApiResource, DynamicObject, GroupVersionKind};
use kube::{Api, Client, Config};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::future::Future;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(200);
const OBSERVE: Duration = Duration::from_secs(180);
const RECORDS: usize = 9;
const CONTAINER: &str = "runner";

/// Builds the client configuration from exactly the listed files, already read with
/// `Kubeconfig::read_from` and merged by `Kubeconfigs::parse`.
pub async fn load_config(kubeconfigs: &Kubeconfigs) -> Result<Config, String> {
    Config::from_custom_kubeconfig(kubeconfigs.merged(), &KubeConfigOptions::default())
        .await
        .map_err(|_| "UNVERIFIED: KUBECONFIG does not describe a usable cluster".to_string())
}

pub struct KubeCluster {
    runtime: tokio::runtime::Runtime,
    config: Config,
    admin: Client,
}

fn failure(error: &kube::Error) -> String {
    match error {
        kube::Error::Api(status) => format!("{}: {}", status.reason, status.message),
        _ => "request failed".to_owned(),
    }
}

impl KubeCluster {
    pub fn connect(kubeconfigs: &Kubeconfigs) -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "UNVERIFIED: cannot start the async runtime".to_string())?;
        let config = runtime.block_on(load_config(kubeconfigs))?;
        let admin = {
            let _guard = runtime.enter();
            Client::try_from(config.clone())
                .map_err(|_| "UNVERIFIED: cannot build the admin client".to_string())?
        };
        Ok(Self {
            runtime,
            config,
            admin,
        })
    }

    fn block<T>(&self, future: impl Future<Output = T>) -> T {
        self.runtime.block_on(future)
    }

    /// A client that carries only the installer token: none of the admin credentials.
    fn installer(&self, token: &Token) -> Result<Client, String> {
        let mut config = self.config.clone();
        config.auth_info = serde_json::from_value::<AuthInfo>(json!({"token": token.expose()}))
            .map_err(|_| "cannot build the installer identity".to_string())?;
        let _guard = self.runtime.enter();
        Client::try_from(config).map_err(|_| "cannot build the installer client".to_string())
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
        let api: Api<KubeNamespace> = Api::all(self.admin.clone());
        let found = self
            .block(api.get_opt(namespace.name()))
            .map_err(|error| failure(&error))?;
        Ok(found.is_none_or(|found| {
            found.metadata.deletion_timestamp.is_some()
                || found.status.and_then(|status| status.phase).as_deref() == Some("Terminating")
        }))
    }
}

fn resource(group: &str, version: &str, kind: &str) -> Result<ApiResource, String> {
    let plural = match kind {
        "ServiceAccount" => "serviceaccounts",
        "Role" => "roles",
        "RoleBinding" => "rolebindings",
        "Deployment" => "deployments",
        "Pod" => "pods",
        _ => return Err(format!("unsupported kind {kind}")),
    };
    Ok(ApiResource::from_gvk_with_plural(
        &GroupVersionKind::gvk(group, version, kind),
        plural,
    ))
}

async fn review(client: Client, group: &str, resource: &str) -> Result<bool, String> {
    let review = SelfSubjectAccessReview {
        spec: SelfSubjectAccessReviewSpec {
            resource_attributes: Some(ResourceAttributes {
                group: Some(group.to_owned()),
                resource: Some(resource.to_owned()),
                verb: Some("create".to_owned()),
                ..ResourceAttributes::default()
            }),
            ..SelfSubjectAccessReviewSpec::default()
        },
        ..SelfSubjectAccessReview::default()
    };
    let answer = Api::<SelfSubjectAccessReview>::all(client)
        .create(&PostParams::default(), &review)
        .await
        .map_err(|error| failure(&error))?;
    Ok(answer.status.is_some_and(|status| status.allowed))
}

impl NamespaceApi for KubeCluster {
    fn can_create_namespaces(&self) -> Result<bool, String> {
        self.block(review(self.admin.clone(), "", "namespaces"))
    }

    fn create_namespace(&self, namespace: &Namespace) -> Result<(), String> {
        let object: KubeNamespace = serde_json::from_value(namespace.manifest())
            .map_err(|_| "namespace manifest is invalid".to_string())?;
        let api: Api<KubeNamespace> = Api::all(self.admin.clone());
        self.block(api.create(&PostParams::default(), &object))
            .map(drop)
            .map_err(|error| failure(&error))
    }

    fn namespace_labels(
        &self,
        namespace: &Namespace,
    ) -> Result<Option<BTreeMap<String, String>>, String> {
        let api: Api<KubeNamespace> = Api::all(self.admin.clone());
        let found = self
            .block(api.get_opt(namespace.name()))
            .map_err(|error| failure(&error))?;
        Ok(found.map(|found| found.metadata.labels.unwrap_or_default()))
    }

    fn delete_namespace(&self, namespace: &Namespace) -> Result<(), String> {
        let api: Api<KubeNamespace> = Api::all(self.admin.clone());
        match self.block(api.delete(namespace.name(), &DeleteParams::default())) {
            Ok(_) => Ok(()),
            Err(kube::Error::Api(status)) if status.code == 404 => Ok(()),
            Err(error) => Err(failure(&error)),
        }
    }

    fn delete_run_objects(&self, namespace: &Namespace, run: &str) -> Result<(), String> {
        let selector = ListParams::default().labels(&format!("{RUN_LABEL}={run}"));
        let mut first_error = None;
        // Deployments first, so no controller recreates what is deleted after them.
        for (group, version, kind) in [
            ("apps", "v1", "Deployment"),
            ("", "v1", "Pod"),
            ("rbac.authorization.k8s.io", "v1", "RoleBinding"),
            ("rbac.authorization.k8s.io", "v1", "Role"),
            ("", "v1", "ServiceAccount"),
        ] {
            let api: Api<DynamicObject> = Api::namespaced_with(
                self.admin.clone(),
                namespace.name(),
                &resource(group, version, kind)?,
            );
            if let Err(error) =
                self.block(api.delete_collection(&DeleteParams::default(), &selector))
            {
                first_error.get_or_insert_with(|| failure(&error));
            }
        }
        first_error.map_or(Ok(()), Err)
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
            Identity::Admin => self.admin.clone(),
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
        let api: Api<ServiceAccount> = Api::namespaced(self.admin.clone(), namespace.name());
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
