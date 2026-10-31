use super::{Namespace, NamespaceMode, validate_spec};
use serde_json::{Value, json};

use std::time::Duration;

pub trait Kube {
    fn create_namespace(&self, namespace: &Namespace) -> Result<(), String>;
    fn create_pod(&self, namespace: &Namespace, pod: &Value) -> Result<(), String>;
    fn wait_terminal(
        &self,
        namespace: &Namespace,
        pod: &str,
        timeout: Duration,
    ) -> Result<String, String>;
    fn logs(&self, namespace: &Namespace, pod: &str) -> Result<String, String>;
    fn delete_namespace(&self, namespace: &Namespace) -> Result<(), String>;
}

pub struct Session<'a, K: Kube> {
    kube: &'a K,
    namespace: Namespace,
    pod: String,
    delete_namespace: bool,
}

impl<'a, K: Kube> Session<'a, K> {
    pub fn close(mut self) -> Result<(), String> {
        if self.delete_namespace {
            self.delete_namespace = false;
            self.kube.delete_namespace(&self.namespace).map_err(|_| {
                format!(
                    "failed to delete guarded namespace {}; operator cleanup required",
                    self.namespace.name()
                )
            })?;
        }
        Ok(())
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
        let run = Namespace::new(name)?;
        let namespace = match mode {
            NamespaceMode::Create => run.clone(),
            NamespaceMode::Shared(namespace) => namespace.clone(),
        };
        let delete_namespace = *mode == NamespaceMode::Create;
        if delete_namespace {
            kube.create_namespace(&namespace).map_err(|_| {
                "FS_TEST_NAMESPACE_MODE=create: namespace creation denied or unavailable"
                    .to_string()
            })?;
        }
        let session = Self {
            kube,
            namespace,
            pod: name.into(),
            delete_namespace,
        };
        kube.create_pod(&session.namespace, &json!({"apiVersion":"v1", "kind":"Pod", "metadata": {
            "name":name, "namespace":session.namespace.name(), "labels":{"falcone.test/run":name}}, "spec":spec}))?;
        Ok(session)
    }
}

impl<K: Kube> Drop for Session<'_, K> {
    fn drop(&mut self) {
        if self.delete_namespace && self.kube.delete_namespace(&self.namespace).is_err() {
            eprintln!(
                "failed to delete guarded namespace {}; operator cleanup required",
                self.namespace.name()
            );
        }
    }
}
