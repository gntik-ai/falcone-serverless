//! Real kube adapter. Credentials never cross the ClusterApi boundary or enter diagnostics.
use super::{
    namespace::Namespace,
    observation::Observation,
    test_namespace::NamespaceApi,
    was01::{ClusterApi, Identity},
};
use k8s_openapi::{
    api::{
        apps::v1::Deployment,
        authentication::v1::{TokenRequest, TokenRequestSpec},
        authorization::v1::{
            ResourceAttributes, SelfSubjectAccessReview, SelfSubjectAccessReviewSpec,
        },
        core::v1::{Namespace as KubeNamespace, Pod, ServiceAccount},
        rbac::v1::{Role, RoleBinding},
    },
    apimachinery::pkg::apis::meta::v1::ObjectMeta,
};
use kube::{
    Api, Client, Config,
    api::{
        DeleteParams, ListParams, LogParams, PostParams, Preconditions, WatchEvent, WatchParams,
    },
    config::{AuthInfo, KubeConfigOptions},
    core::{ApiResource, DynamicObject, Request},
};
use serde_json::Value;
use std::{
    cell::RefCell,
    time::{Duration, Instant},
};
use tokio::runtime::{Builder, Runtime};

pub struct Clients {
    runtime: Runtime,
    config: Config,
    admin: Client,
    installer: RefCell<Option<Client>>,
    namespace: RefCell<Option<String>>,
}

fn api_error(operation: &str, error: kube::Error) -> String {
    // Never format arbitrary client errors: they can include authentication configuration.
    match error {
        kube::Error::Api(response) => {
            format!("{operation}: API {} {}", response.code, response.reason)
        }
        _ => format!("{operation}: Kubernetes request unavailable"),
    }
}

fn preconditions(metadata: &ObjectMeta) -> Result<DeleteParams, String> {
    let uid = metadata.uid.clone().ok_or("cleanup UID unavailable")?;
    let version = metadata
        .resource_version
        .clone()
        .ok_or("cleanup resourceVersion unavailable")?;
    Ok(DeleteParams::background().preconditions(Preconditions {
        uid: Some(uid),
        resource_version: Some(version),
    }))
}

impl Clients {
    pub fn new() -> Result<Self, String> {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "UNVERIFIED: async runtime unavailable")?;
        let (config, admin) = runtime.block_on(async {
            // Strict kubeconfig loading: no in-cluster or other identity fallback.
            let mut config = Config::from_kubeconfig(&KubeConfigOptions::default())
                .await
                .map_err(|_| "UNVERIFIED: KUBECONFIG unavailable")?;
            config.connect_timeout = Some(Duration::from_secs(10));
            config.read_timeout = Some(Duration::from_secs(15));
            config.write_timeout = Some(Duration::from_secs(15));
            let admin = Client::try_from(config.clone())
                .map_err(|_| "UNVERIFIED: admin client unavailable")?;
            Ok::<_, String>((config, admin))
        })?;
        Ok(Self {
            runtime,
            config,
            admin,
            installer: RefCell::new(None),
            namespace: RefCell::new(None),
        })
    }

    fn bind_namespace(&self, namespace: &Namespace) -> Result<(), String> {
        let mut selected = self.namespace.borrow_mut();
        if selected
            .as_deref()
            .is_some_and(|name| name != namespace.name())
        {
            return Err("refused another namespace".into());
        }
        *selected = Some(namespace.name().into());
        Ok(())
    }

    fn installer(&self) -> Result<Client, String> {
        self.installer
            .borrow()
            .clone()
            .ok_or("UNVERIFIED: installer identity unavailable".into())
    }

    fn objects(
        &self,
        client: Client,
        namespace: &Namespace,
        kind: &str,
    ) -> Result<Api<DynamicObject>, String> {
        self.bind_namespace(namespace)?;
        let resource = match kind {
            "ServiceAccount" => ApiResource::erase::<ServiceAccount>(&()),
            "Deployment" => ApiResource::erase::<Deployment>(&()),
            "Role" => ApiResource::erase::<Role>(&()),
            "RoleBinding" => ApiResource::erase::<RoleBinding>(&()),
            _ => return Err("refused non-fixture kind".into()),
        };
        Ok(Api::namespaced_with(client, namespace.name(), &resource))
    }

    fn review(&self, client: Client, group: &str, resource: &str) -> Result<bool, String> {
        self.runtime.block_on(async {
            let reviews: Api<SelfSubjectAccessReview> = Api::all(client);
            let review = SelfSubjectAccessReview {
                spec: SelfSubjectAccessReviewSpec {
                    resource_attributes: Some(ResourceAttributes {
                        group: Some(group.into()),
                        resource: Some(resource.into()),
                        verb: Some("create".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                ..Default::default()
            };
            let response = reviews
                .create(&PostParams::default(), &review)
                .await
                .map_err(|error| api_error("authorization review", error))?;
            let status = response
                .status
                .ok_or("UNVERIFIED: authorization result unavailable")?;
            if status
                .evaluation_error
                .as_ref()
                .is_some_and(|error| !error.is_empty())
            {
                return Err("UNVERIFIED: authorization evaluation failed".into());
            }
            Ok(status.allowed)
        })
    }
}

impl NamespaceApi for Clients {
    fn can_create_namespace(&self) -> Result<bool, String> {
        self.review(self.admin.clone(), "", "namespaces")
    }

    fn get_namespace(&self, namespace: &Namespace) -> Result<Option<Value>, String> {
        self.bind_namespace(namespace)?;
        let api: Api<KubeNamespace> = Api::all(self.admin.clone());
        let object = self
            .runtime
            .block_on(api.get_opt(namespace.name()))
            .map_err(|error| api_error("namespace lookup", error))?;
        object
            .map(serde_json::to_value)
            .transpose()
            .map_err(|_| "namespace evidence unavailable".into())
    }

    fn create_namespace(&self, object: &Value) -> Result<(), String> {
        let namespace = Namespace::new(
            object["metadata"]["name"]
                .as_str()
                .ok_or("namespace name missing")?,
        )?;
        self.bind_namespace(&namespace)?;
        let object: KubeNamespace =
            serde_json::from_value(object.clone()).map_err(|_| "namespace invalid")?;
        let api: Api<KubeNamespace> = Api::all(self.admin.clone());
        self.runtime
            .block_on(api.create(&PostParams::default(), &object))
            .map_err(|error| api_error("namespace create", error))?;
        Ok(())
    }

    fn delete_namespace(&self, namespace: &Namespace, run: &str) -> Result<(), String> {
        self.bind_namespace(namespace)?;
        self.runtime.block_on(async {
            let api: Api<KubeNamespace> = Api::all(self.admin.clone());
            if let Some(current) = api
                .get_opt(namespace.name())
                .await
                .map_err(|error| api_error("namespace cleanup lookup", error))?
            {
                if current
                    .metadata
                    .labels
                    .as_ref()
                    .and_then(|labels| labels.get("falcone.test/run"))
                    .map(String::as_str)
                    != Some(run)
                {
                    return Err("refused namespace cleanup with another run label".into());
                }
                api.delete(namespace.name(), &preconditions(&current.metadata)?)
                    .await
                    .map_err(|error| api_error("namespace cleanup", error))?;
                if api
                    .get_opt(namespace.name())
                    .await
                    .map_err(|error| api_error("namespace deletion evidence", error))?
                    .is_some_and(|namespace| namespace.metadata.deletion_timestamp.is_none())
                {
                    return Err(
                        "namespace is neither NotFound nor Terminating after cleanup".into(),
                    );
                }
            }
            Ok(())
        })
    }

    fn get_object(&self, namespace: &Namespace, object: &Value) -> Result<Option<Value>, String> {
        let api = self.objects(
            self.admin.clone(),
            namespace,
            object["kind"].as_str().ok_or("cleanup kind missing")?,
        )?;
        let name = object["metadata"]["name"]
            .as_str()
            .ok_or("cleanup name missing")?;
        let current = self
            .runtime
            .block_on(api.get_opt(name))
            .map_err(|error| api_error("cleanup lookup", error))?;
        current
            .map(serde_json::to_value)
            .transpose()
            .map_err(|_| "cleanup metadata unavailable".into())
    }

    fn delete_object(&self, namespace: &Namespace, object: &Value) -> Result<(), String> {
        let api = self.objects(
            self.admin.clone(),
            namespace,
            object["kind"].as_str().ok_or("cleanup kind missing")?,
        )?;
        let current: DynamicObject =
            serde_json::from_value(object.clone()).map_err(|_| "cleanup metadata invalid")?;
        let name = current
            .metadata
            .name
            .as_deref()
            .ok_or("cleanup name missing")?;
        self.runtime
            .block_on(api.delete(name, &preconditions(&current.metadata)?))
            .map_err(|error| api_error("object cleanup", error))?;
        Ok(())
    }
}

impl ClusterApi for Clients {
    fn create(&self, identity: Identity, object: &Value) -> Result<Value, String> {
        let namespace = Namespace::new(
            object["metadata"]["namespace"]
                .as_str()
                .ok_or("namespace missing")?,
        )?;
        let kind = object["kind"].as_str().ok_or("kind missing")?;
        let installer_account = kind == "ServiceAccount"
            && object["metadata"]["name"]
                .as_str()
                .is_some_and(|name| name.starts_with("was01-installer-"));
        let expected = if installer_account || matches!(kind, "Role" | "RoleBinding") {
            Identity::Admin
        } else {
            Identity::Installer
        };
        if identity != expected {
            return Err("refused wrong phase identity".into());
        }
        let client = match identity {
            Identity::Admin => self.admin.clone(),
            Identity::Installer => self.installer()?,
        };
        let api = self.objects(client, &namespace, kind)?;
        let object: DynamicObject =
            serde_json::from_value(object.clone()).map_err(|_| "fixture invalid")?;
        let admitted = self
            .runtime
            .block_on(api.create(&PostParams::default(), &object))
            .map_err(|error| match error {
                // Only fixture requests reach this branch: neither request nor response carries a credential.
                kube::Error::Api(response) => format!(
                    "admission: API {} {}: {}",
                    response.code, response.reason, response.message
                ),
                error => api_error("admission", error),
            })?;
        serde_json::to_value(admitted).map_err(|_| "admission response unavailable".into())
    }

    fn installer_token(&self, namespace: &Namespace, account: &str) -> Result<(), String> {
        self.bind_namespace(namespace)?;
        let client = self.runtime.block_on(async {
            let api: Api<ServiceAccount> = Api::namespaced(self.admin.clone(), namespace.name());
            let request = TokenRequest {
                spec: TokenRequestSpec {
                    audiences: vec![],
                    expiration_seconds: Some(600),
                    ..Default::default()
                },
                ..Default::default()
            };
            let response = api
                .create_token_request(account, &PostParams::default(), &request)
                .await
                .map_err(|error| api_error("TokenRequest", error))?;
            let token = response
                .status
                .ok_or("UNVERIFIED: TokenRequest status missing")?
                .token;
            if token.is_empty() {
                return Err("UNVERIFIED: TokenRequest token missing".into());
            }
            let mut config = self.config.clone();
            // Remove admin certs, impersonation, plugins and custom headers before installing the token.
            config.auth_info = AuthInfo {
                token: Some(token.into()),
                ..Default::default()
            };
            config.headers.clear();
            Client::try_from(config)
                .map_err(|_| "UNVERIFIED: installer client unavailable".to_string())
        })?;
        *self.installer.borrow_mut() = Some(client);
        Ok(())
    }

    fn installer_can_create(&self, group: &str, resource: &str) -> Result<bool, String> {
        self.review(self.installer()?, group, resource)
    }

    fn begin_observation(&self, namespace: &Namespace, run: &str) -> Result<String, String> {
        self.bind_namespace(namespace)?;
        let api: Api<Pod> = Api::namespaced(self.installer()?, namespace.name());
        let list = self
            .runtime
            .block_on(api.list(&ListParams::default().labels(&format!("falcone.test/run={run}"))))
            .map_err(|error| api_error("pod watch baseline", error))?;
        if !list.items.is_empty() {
            return Err("UNVERIFIED: pod already exists for this run".into());
        }
        list.metadata
            .resource_version
            .ok_or("UNVERIFIED: pod watch resourceVersion missing".into())
    }

    fn observe(
        &self,
        namespace: &Namespace,
        run: &str,
        version: &str,
    ) -> Result<Observation, String> {
        self.bind_namespace(namespace)?;
        let client = self.installer()?;
        let pods: Api<Pod> = Api::namespaced(client.clone(), namespace.name());
        self.runtime.block_on(async {
            let deadline = Instant::now() + Duration::from_secs(180);
            let mut version = version.to_owned();
            let mut observation = Observation::default();
            while Instant::now() < deadline {
                // Buffer one second of watch events, not a late snapshot. This needs no extra stream crate.
                let request = Request::new(pods.resource_url())
                    .watch(
                        &WatchParams::default()
                            .labels(&format!("falcone.test/run={run}"))
                            .timeout(1),
                        &version,
                    )
                    .map_err(|_| "pod watch request invalid")?;
                let events = client
                    .request_text(request)
                    .await
                    .map_err(|error| api_error("pod watch", error))?;
                for line in events.lines().filter(|line| !line.trim().is_empty()) {
                    let event: WatchEvent<Pod> =
                        serde_json::from_str(line).map_err(|_| "pod watch event invalid")?;
                    match event {
                        WatchEvent::Added(pod) | WatchEvent::Modified(pod) => {
                            // Preserve startup/termination messages even if qualification later fails.
                            println!(
                                "{}",
                                serde_json::json!({"pod": pod.metadata.name, "status": pod.status})
                            );
                            version = pod
                                .metadata
                                .resource_version
                                .clone()
                                .ok_or("pod watch version missing")?;
                            observation.record(&pod)?;
                        }
                        WatchEvent::Bookmark(bookmark) => {
                            version = bookmark.metadata.resource_version
                        }
                        WatchEvent::Deleted(_) => {
                            return Err("fixture pod deleted before qualification completed".into());
                        }
                        WatchEvent::Error(status) => {
                            return Err(format!("pod watch API error {}", status.code));
                        }
                    }
                }
                if observation.exit_code.is_some() {
                    let name = observation
                        .pod_name
                        .as_deref()
                        .ok_or("fixture pod name missing")?;
                    observation.logs = pods
                        .logs(
                            name,
                            &LogParams {
                                container: Some("runner".into()),
                                limit_bytes: Some(65536),
                                ..Default::default()
                            },
                        )
                        .await
                        .map_err(|error| api_error("runner logs", error))?;
                    return Ok(observation);
                }
            }
            Err("fixture startup/execution not completed within 180 seconds".into())
        })
    }
}
