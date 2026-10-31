//! Opt-in Kubernetes transport. No tokens in argv, logs, errors or persisted evidence.
use super::was01::{Identity, Plan, QualificationClient};
use super::{Namespace, NamespaceClient};
use k8s_openapi::api::{
    authentication::v1::{TokenRequest, TokenRequestSpec},
    authorization::v1::{ResourceAttributes, SelfSubjectAccessReview, SelfSubjectAccessReviewSpec},
    core::v1::{Namespace as KubeNamespace, Pod, ServiceAccount},
};
use kube::{
    Api, Client, Config,
    api::{DeleteParams, DynamicObject, ListParams, PostParams},
    config::{AuthInfo, KubeConfigOptions},
    core::{ApiResource, GroupVersionKind},
};
use serde_json::Value;
use std::{cell::RefCell, time::Duration};
use tokio::runtime::Runtime;

pub struct Live {
    runtime: Runtime,
    admin: Client,
    installer: RefCell<Option<Client>>,
    config: Config,
}

const API_ERROR: &str = "Kubernetes request failed; response and credentials suppressed";

impl Live {
    pub fn new() -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "UNVERIFIED: cannot create test runtime")?;
        // Explicit kubeconfig only, no in-cluster/ambient fallback or debug overrides.
        let (config, admin) = runtime.block_on(async {
            let mut config = Config::from_kubeconfig(&KubeConfigOptions::default())
                .await
                .map_err(|_| "UNVERIFIED: KUBECONFIG unavailable (value suppressed)")?;
            config.connect_timeout = Some(Duration::from_secs(15));
            config.read_timeout = Some(Duration::from_secs(15));
            config.write_timeout = Some(Duration::from_secs(15));
            let client = Client::try_from(config.clone())
                .map_err(|_| "UNVERIFIED: admin client unavailable")?;
            Ok::<_, String>((config, client))
        })?;
        Ok(Self {
            runtime,
            admin,
            installer: RefCell::new(None),
            config,
        })
    }

    fn client(&self, identity: Identity) -> Result<Client, String> {
        match identity {
            Identity::Admin => Ok(self.admin.clone()),
            Identity::Installer => self
                .installer
                .borrow()
                .clone()
                .ok_or_else(|| "installer token is unavailable".into()),
        }
    }

    fn api(
        &self,
        client: Client,
        namespace: &Namespace,
        group: &str,
        version: &str,
        kind: &str,
    ) -> Api<DynamicObject> {
        let resource = ApiResource::from_gvk(&GroupVersionKind::gvk(group, version, kind));
        Api::namespaced_with(client, namespace.name(), &resource)
    }

    fn ssar(&self, client: Client, group: &str, resource: &str) -> Result<bool, String> {
        let api: Api<SelfSubjectAccessReview> = Api::all(client);
        let review = SelfSubjectAccessReview {
            spec: SelfSubjectAccessReviewSpec {
                resource_attributes: Some(ResourceAttributes {
                    verb: Some("create".into()),
                    group: Some(group.into()),
                    resource: Some(resource.into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        let response = self
            .runtime
            .block_on(api.create(&PostParams::default(), &review))
            .map_err(|_| API_ERROR)?;
        let status = response.status.ok_or("SSAR status missing")?;
        if status
            .evaluation_error
            .is_some_and(|error| !error.is_empty())
        {
            return Err("SSAR evaluation unverified".into());
        }
        Ok(status.allowed && status.denied != Some(true))
    }
}

impl NamespaceClient for Live {
    fn can_create_namespace(&self) -> Result<bool, String> {
        self.ssar(self.admin.clone(), "", "namespaces")
    }

    fn get_namespace(&self, namespace: &Namespace) -> Result<Value, String> {
        let api: Api<KubeNamespace> = Api::all(self.admin.clone());
        let ns = self
            .runtime
            .block_on(api.get(namespace.name()))
            .map_err(|_| API_ERROR)?;
        serde_json::to_value(ns).map_err(|_| API_ERROR.into())
    }

    fn create_namespace(&self, namespace: &Namespace) -> Result<(), String> {
        let api: Api<KubeNamespace> = Api::all(self.admin.clone());
        let ns: KubeNamespace =
            serde_json::from_value(namespace.manifest()).map_err(|_| "invalid namespace")?;
        self.runtime
            .block_on(api.create(&PostParams::default(), &ns))
            .map_err(|_| API_ERROR)?;
        Ok(())
    }

    fn delete_namespace(&self, namespace: &Namespace) -> Result<(), String> {
        let api: Api<KubeNamespace> = Api::all(self.admin.clone());
        self.runtime.block_on(async {
            match api.delete(namespace.name(), &DeleteParams::default()).await {
                Ok(_) => {}
                Err(kube::Error::Api(error)) if error.code == 404 => return Ok(()),
                Err(_) => return Err(API_ERROR.into()),
            }
            let ns = api.get_opt(namespace.name()).await.map_err(|_| API_ERROR)?;
            if ns.is_some_and(|ns| ns.metadata.deletion_timestamp.is_none()) {
                return Err("namespace remains active after cleanup".into());
            }
            Ok(())
        })
    }

    fn cleanup_run(&self, namespace: &Namespace, run: &str) -> Result<(), String> {
        self.runtime
            .block_on(super::cleanup::cleanup_run(self, namespace, run))
    }
}

impl QualificationClient for Live {
    fn create(
        &self,
        identity: Identity,
        namespace: &Namespace,
        object: &Value,
    ) -> Result<Value, String> {
        let api_version = object["apiVersion"].as_str().ok_or("apiVersion missing")?;
        let (group, version) = api_version.split_once('/').unwrap_or(("", api_version));
        let kind = object["kind"].as_str().ok_or("kind missing")?;
        let api = self.api(self.client(identity)?, namespace, group, version, kind);
        let object: DynamicObject =
            serde_json::from_value(object.clone()).map_err(|_| "invalid object")?;
        let response = self
            .runtime
            .block_on(api.create(&PostParams::default(), &object))
            .map_err(|_| API_ERROR)?;
        serde_json::to_value(response).map_err(|_| API_ERROR.into())
    }

    fn token_request(&self, namespace: &Namespace, serviceaccount: &str) -> Result<(), String> {
        let api: Api<ServiceAccount> = Api::namespaced(self.admin.clone(), namespace.name());
        let request = TokenRequest {
            spec: TokenRequestSpec {
                audiences: vec![],
                expiration_seconds: Some(600),
                ..Default::default()
            },
            ..Default::default()
        };
        self.runtime.block_on(async {
            let response = api
                .create_token_request(serviceaccount, &PostParams::default(), &request)
                .await
                .map_err(|_| API_ERROR)?;
            let token = response.status.ok_or("TokenRequest status missing")?.token;
            if token.is_empty() {
                return Err("TokenRequest token missing".into());
            }
            // New auth info and empty headers: no admin cert, exec plugin, token file,
            // basic auth or impersonation survives into the installer identity.
            let mut config = Config::new(self.config.cluster_url.clone());
            config.root_cert = self.config.root_cert.clone();
            config.tls_server_name = self.config.tls_server_name.clone();
            config.proxy_url = self.config.proxy_url.clone();
            config.connect_timeout = Some(Duration::from_secs(15));
            config.read_timeout = Some(Duration::from_secs(15));
            config.write_timeout = Some(Duration::from_secs(15));
            config.auth_info = AuthInfo {
                token: Some(token.into()),
                ..Default::default()
            };
            let client = Client::try_from(config).map_err(|_| "installer client unavailable")?;
            *self.installer.borrow_mut() = Some(client);
            Ok(())
        })
    }

    fn installer_can_create(&self, group: &str, resource: &str) -> Result<bool, String> {
        self.ssar(self.client(Identity::Installer)?, group, resource)
    }

    fn observe(&self, plan: &Plan) -> Result<String, String> {
        let namespace = &plan.namespace;
        let client = self.client(Identity::Installer)?;
        let api: Api<Pod> = Api::namespaced(client, namespace.name());
        self.runtime
            .block_on(super::observation::observe(&api, plan))
    }
}

impl super::cleanup::CleanupClient for Live {
    async fn list(
        &self,
        namespace: &Namespace,
        resource: (&str, &str, &str),
        params: &ListParams,
    ) -> Result<kube::core::ObjectList<DynamicObject>, kube::Error> {
        self.api(
            self.admin.clone(),
            namespace,
            resource.0,
            resource.1,
            resource.2,
        )
        .list(params)
        .await
    }
    async fn get(
        &self,
        namespace: &Namespace,
        resource: (&str, &str, &str),
        name: &str,
    ) -> Result<Option<DynamicObject>, kube::Error> {
        self.api(
            self.admin.clone(),
            namespace,
            resource.0,
            resource.1,
            resource.2,
        )
        .get_opt(name)
        .await
    }
    async fn delete(
        &self,
        namespace: &Namespace,
        resource: (&str, &str, &str),
        name: &str,
        params: &DeleteParams,
    ) -> Result<(), kube::Error> {
        self.api(
            self.admin.clone(),
            namespace,
            resource.0,
            resource.1,
            resource.2,
        )
        .delete(name, params)
        .await
        .map(|_| ())
    }
}
