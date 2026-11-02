use super::{Namespace, NamespaceApi, NamespaceMode, NamespacePlan, RUN_LABEL, TestNamespace, validate_spec};
use serde_json::{Value, json};

use std::time::Duration;

pub trait Kube: NamespaceApi {
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
    namespace: TestNamespace<'a, K>,
    pod: String,
}

impl<'a, K: Kube> Session<'a, K> {
    pub fn close(self) -> Result<(), String> {
        self.namespace.close()
    }

    pub fn verify(&self) -> Result<(), String> {
        let namespace = self.namespace.namespace();
        let phase = self
            .kube
            .wait_terminal(namespace, &self.pod, Duration::from_secs(180))?;
        if phase != "Succeeded" {
            return Err("worker pod did not succeed".into());
        }
        let logs = self.kube.logs(namespace, &self.pod)?;
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
        Namespace::new(name)?;
        let plan = NamespacePlan::for_run(name, mode)?;
        // The pod carries the label that the namespace guard's cleanup selects by.
        let run = plan.run().to_owned();
        let target = plan.namespace().name().to_owned();
        let namespace = plan.acquire(kube)?;
        kube.create_pod(
            namespace.namespace(),
            &json!({"apiVersion":"v1", "kind":"Pod", "metadata": {
            "name":name, "namespace":target, "labels":{RUN_LABEL:run}}, "spec":spec}),
        )?;
        Ok(Self {
            kube,
            namespace,
            pod: name.into(),
        })
    }
}
