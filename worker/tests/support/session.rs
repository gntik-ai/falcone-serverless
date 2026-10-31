use super::{Namespace, NamespaceClient, NamespaceGuard, NamespaceMode, validate_spec};
use serde_json::{Value, json};

use std::time::Duration;

pub trait Kube: NamespaceClient {
    fn create_pod(&self, namespace: &Namespace, pod: &Value) -> Result<(), String>;
    fn wait_terminal(
        &self,
        namespace: &Namespace,
        pod: &str,
        timeout: Duration,
    ) -> Result<String, String>;
    fn logs(&self, namespace: &Namespace, pod: &str) -> Result<String, String>;
}

pub struct Session<'a, K: Kube> {
    kube: &'a K,
    namespace: Namespace,
    pod: String,
    guard: NamespaceGuard<'a, K>,
}

impl<'a, K: Kube> Session<'a, K> {
    pub fn close(self) -> Result<(), String> {
        self.guard.close()
    }

    pub fn verify(&self) -> Result<(), String> {
        let phase =
            self.kube
                .wait_terminal(&self.namespace, &self.pod, Duration::from_secs(180))?;
        if phase != "Succeeded" {
            return Err("worker pod did not succeed".into());
        }
        let logs = self.kube.logs(&self.namespace, &self.pod)?;
        if !logs.contains("falcone-serverless-worker 0.1.0") {
            return Err("worker version log is missing".into());
        }
        Ok(())
    }

    pub fn create(kube: &'a K, name: &str, spec: &Value) -> Result<Self, String> {
        Self::create_in(kube, name, spec, &NamespaceMode::Create)
    }

    pub fn create_in(
        kube: &'a K,
        name: &str,
        spec: &Value,
        mode: &NamespaceMode,
    ) -> Result<Self, String> {
        validate_spec(spec)?;
        let guard = NamespaceGuard::open(kube, name, mode)?;
        let session = Self {
            kube,
            namespace: guard.namespace.clone(),
            pod: name.into(),
            guard,
        };
        kube.create_pod(&session.namespace, &json!({"apiVersion":"v1", "kind":"Pod", "metadata": {
            "name":name, "namespace":session.namespace.name(), "labels":{"falcone.test/run":name}}, "spec":spec}))?;
        Ok(session)
    }
}
