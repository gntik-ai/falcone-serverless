//! Reusable namespace lease for cluster suites. No ambient environment is read here.
use super::namespace::{Namespace, NamespaceMode};
use serde_json::Value;

/// Kubernetes API boundary, separate from the legacy worker-image Kube/Session API.
pub trait NamespaceApi {
    fn can_create_namespace(&self) -> Result<bool, String>;
    fn get_namespace(&self, namespace: &Namespace) -> Result<Option<Value>, String>;
    fn create_namespace(&self, object: &Value) -> Result<(), String>;
    fn delete_namespace(&self, namespace: &Namespace, run: &str) -> Result<(), String>;
    fn get_object(&self, namespace: &Namespace, object: &Value) -> Result<Option<Value>, String>;
    fn delete_object(&self, namespace: &Namespace, object: &Value) -> Result<(), String>;
}

pub struct NamespaceGuard<'a, K: NamespaceApi> {
    kube: &'a K,
    pub namespace: Namespace,
    pub run: String,
    created: bool,
    closed: bool,
    objects: Vec<Value>,
}

pub fn test_namespace<'a, K: NamespaceApi>(
    kube: &'a K,
    suite: &str,
    mode: &NamespaceMode,
) -> Result<NamespaceGuard<'a, K>, String> {
    let run = super::namespace::namespace_name()[8..].to_owned();
    let generated = Namespace::new(&format!("fs-test-{suite}-{run}"))?;
    let namespace = match mode {
        NamespaceMode::Create => {
            if !kube.can_create_namespace().unwrap_or(false) {
                return Err("UNVERIFIED: namespace create authorization unavailable or denied; use FS_TEST_NAMESPACE_MODE=shared".into());
            }
            generated
        }
        NamespaceMode::Shared(namespace) => {
            let object = kube
                .get_namespace(namespace)
                .map_err(|error| format!("UNVERIFIED: shared namespace lookup: {error}"))?
                .ok_or("UNVERIFIED: FS_TEST_NAMESPACE does not exist")?;
            if object["metadata"]["name"] != namespace.name()
                || object["metadata"]["labels"]["pod-security.kubernetes.io/enforce"]
                    != "restricted"
                || !object["metadata"]["deletionTimestamp"].is_null()
            {
                return Err(
                    "UNVERIFIED: FS_TEST_NAMESPACE must be active and enforce=restricted".into(),
                );
            }
            namespace.clone()
        }
    };
    let guard = NamespaceGuard {
        kube,
        namespace,
        run,
        created: *mode == NamespaceMode::Create,
        closed: false,
        objects: Vec::new(),
    };
    if guard.created {
        // Arm cleanup before the request: the server may create it even if its response is lost.
        let mut object = guard.namespace.manifest();
        object["metadata"]["labels"]["falcone.test/run"] = guard.run.clone().into();
        kube.create_namespace(&object)
            .map_err(|error| format!("UNVERIFIED: {error}; use FS_TEST_NAMESPACE_MODE=shared"))?;
    }
    Ok(guard)
}

impl<K: NamespaceApi> NamespaceGuard<'_, K> {
    pub fn track(&mut self, object: &Value) -> Result<(), String> {
        if object["metadata"]["namespace"] != self.namespace.name()
            || object["metadata"]["labels"]["falcone.test/run"] != self.run
            || !object["metadata"]["name"]
                .as_str()
                .is_some_and(|name| name.ends_with(&format!("-{}", self.run)))
        {
            return Err("refused object outside this namespace/run".into());
        }
        self.objects.push(object.clone());
        Ok(())
    }

    pub fn close(mut self) -> Result<(), String> {
        self.cleanup()
    }

    fn cleanup(&mut self) -> Result<(), String> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        if self.created {
            self.kube.delete_namespace(&self.namespace, &self.run)?;
        }
        let mut errors = Vec::new();
        if !self.created {
            for object in self.objects.iter().rev() {
                match self.kube.get_object(&self.namespace, object) {
                    Ok(Some(current))
                        if current["metadata"]["labels"]["falcone.test/run"] == self.run =>
                    {
                        if let Err(error) = self.kube.delete_object(&self.namespace, &current) {
                            errors.push(error);
                        }
                    }
                    Ok(None) => {}
                    Ok(Some(_)) => {
                        errors.push("refused cleanup of object with another run label".into())
                    }
                    Err(error) => errors.push(error),
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}

impl<K: NamespaceApi> Drop for NamespaceGuard<'_, K> {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            eprintln!("UNVERIFIED: namespace guard cleanup failed: {error}");
        }
    }
}
