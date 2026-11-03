//! The cluster core shared by the WAS-01 and WAS-02 harnesses: one client construction from
//! exactly the listed kubeconfig files (see `kubeconfig.rs`) and one `NamespaceApi`. `Config::infer`,
//! the default `from_kubeconfig`, the in-cluster configuration and `~/.kube/config` are never
//! referenced. Credentials are neither printed nor logged; errors carry the API status message
//! only.

use super::{Kubeconfigs, Namespace, NamespaceApi, RUN_LABEL};
use k8s_openapi::api::authorization::v1::{
    ResourceAttributes, SelfSubjectAccessReview, SelfSubjectAccessReviewSpec,
};
use k8s_openapi::api::core::v1::Namespace as KubeNamespace;
use kube::api::{DeleteParams, ListParams, PostParams};
use kube::config::KubeConfigOptions;
use kube::core::{ApiResource, DynamicObject, GroupVersionKind};
use kube::{Api, Client, Config};
use std::collections::BTreeMap;
use std::future::Future;

/// Builds the client configuration from exactly the listed files, already read with
/// `Kubeconfig::read_from` and merged by `Kubeconfigs::parse`.
pub async fn load_config(kubeconfigs: &Kubeconfigs) -> Result<Config, String> {
    Config::from_custom_kubeconfig(kubeconfigs.merged(), &KubeConfigOptions::default())
        .await
        .map_err(|_| "UNVERIFIED: KUBECONFIG does not describe a usable cluster".to_string())
}

pub fn failure(error: &kube::Error) -> String {
    match error {
        kube::Error::Api(status) => format!("{}: {}", status.reason, status.message),
        _ => "request failed".to_owned(),
    }
}

pub fn resource(group: &str, version: &str, kind: &str) -> Result<ApiResource, String> {
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

pub async fn review(client: Client, group: &str, resource: &str) -> Result<bool, String> {
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

/// The test-admin identity on the cluster, with its own current-thread runtime.
pub struct Cluster {
    runtime: tokio::runtime::Runtime,
    admin: Client,
}

impl Cluster {
    /// Also returns the client configuration, for callers that build further identities.
    pub fn connect(kubeconfigs: &Kubeconfigs) -> Result<(Self, Config), String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "UNVERIFIED: cannot start the async runtime".to_string())?;
        let config = runtime.block_on(load_config(kubeconfigs))?;
        let admin = runtime
            .block_on(async { Client::try_from(config.clone()) })
            .map_err(|_| "UNVERIFIED: cannot build the admin client".to_string())?;
        Ok((Self { runtime, admin }, config))
    }

    pub fn block<T>(&self, future: impl Future<Output = T>) -> T {
        self.runtime.block_on(future)
    }

    pub fn admin(&self) -> &Client {
        &self.admin
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

impl NamespaceApi for Cluster {
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
