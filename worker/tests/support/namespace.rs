use serde_json::{Value, json};

pub fn namespace_name() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::BuildHasher;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let counter = NEXT.fetch_add(1, Ordering::Relaxed);
    let random = RandomState::new().hash_one((std::process::id(), counter));
    format!("fs-test-{random:016x}-{counter:x}")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Namespace(String);

impl Namespace {
    pub fn new(name: &str) -> Result<Self, String> {
        if !name.starts_with("fs-test-")
            || name.len() <= 8
            || name.len() > 63
            || !name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || !name
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric)
        {
            return Err(
                "namespace must be an fs-test-* DNS-1123 label of at most 63 characters".into(),
            );
        }
        Ok(Self(name.into()))
    }

    pub fn name(&self) -> &str {
        &self.0
    }

    pub fn manifest(&self) -> Value {
        json!({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": self.0,
            "labels": {"pod-security.kubernetes.io/enforce": "restricted"}}})
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NamespaceMode {
    Create,
    Shared(Namespace),
}

impl NamespaceMode {
    pub fn parse(mode: Option<&str>, namespace: Option<&str>) -> Result<Self, String> {
        match mode {
            None | Some("create") => Ok(Self::Create),
            Some("shared") => {
                let namespace = namespace
                    .ok_or("FS_TEST_NAMESPACE is required for FS_TEST_NAMESPACE_MODE=shared")?;
                Ok(Self::Shared(Namespace::new(namespace)?))
            }
            _ => Err("FS_TEST_NAMESPACE_MODE must be create or shared".into()),
        }
    }
}

/// API boundary shared by every cluster fixture. Implementations never expose credentials.
pub trait NamespaceClient {
    fn can_create_namespace(&self) -> Result<bool, String>;
    fn get_namespace(&self, namespace: &Namespace) -> Result<Value, String>;
    fn create_namespace(&self, namespace: &Namespace) -> Result<(), String>;
    fn delete_namespace(&self, namespace: &Namespace) -> Result<(), String>;
    fn cleanup_run(&self, namespace: &Namespace, run: &str) -> Result<(), String>;
}

/// The one namespace lifecycle/ownership guard for cluster tests.
pub struct NamespaceGuard<'a, K: NamespaceClient> {
    pub namespace: Namespace,
    pub run: String,
    kube: &'a K,
    create: bool,
    active: bool,
}

pub fn test_namespace<'a, K: NamespaceClient>(
    kube: &'a K,
    suite: &str,
    mode: &NamespaceMode,
) -> Result<NamespaceGuard<'a, K>, String> {
    let run = format!("fs-test-{suite}-{:016x}", rand::random::<u64>());
    NamespaceGuard::open(kube, &run, mode)
}

impl<'a, K: NamespaceClient> NamespaceGuard<'a, K> {
    pub fn open(kube: &'a K, run: &str, mode: &NamespaceMode) -> Result<Self, String> {
        Namespace::new(run)?;
        let namespace = match mode {
            NamespaceMode::Create => {
                if !kube.can_create_namespace().unwrap_or(false) {
                    return Err("UNVERIFIED: namespace-create SSAR denied/unavailable; explicitly authorize FS_TEST_NAMESPACE_MODE=shared to use an existing fs-test-* namespace".into());
                }
                Namespace::new(run)?
            }
            NamespaceMode::Shared(namespace) => {
                let object = kube.get_namespace(namespace).map_err(
                    |_| "UNVERIFIED: FS_TEST_NAMESPACE must exist and enforce restricted",
                )?;
                if object["metadata"]["name"] != namespace.name()
                    || object["metadata"]["labels"]["pod-security.kubernetes.io/enforce"]
                        != "restricted"
                    || !object["metadata"]["deletionTimestamp"].is_null()
                {
                    return Err("UNVERIFIED: FS_TEST_NAMESPACE must exist, be active and enforce restricted".into());
                }
                namespace.clone()
            }
        };
        let create = *mode == NamespaceMode::Create;
        if create {
            kube.create_namespace(&namespace).map_err(
                |_| "UNVERIFIED: FS_TEST_NAMESPACE_MODE=create namespace creation failed",
            )?;
        }
        let guard = Self {
            namespace,
            run: run.into(),
            kube,
            create,
            active: true,
        };
        if create {
            let admitted = kube
                .get_namespace(&guard.namespace)
                .map_err(|_| "UNVERIFIED: created namespace admission unavailable")?;
            if admitted["metadata"]["name"] != guard.namespace.name()
                || admitted["metadata"]["labels"]["pod-security.kubernetes.io/enforce"]
                    != "restricted"
                || !admitted["metadata"]["deletionTimestamp"].is_null()
            {
                return Err(
                    "UNVERIFIED: created namespace did not retain restricted admission".into(),
                );
            }
        }
        Ok(guard)
    }

    pub fn close(mut self) -> Result<(), String> {
        self.active = false;
        self.cleanup()
    }

    fn cleanup(&self) -> Result<(), String> {
        let result = if self.create {
            self.kube.delete_namespace(&self.namespace)
        } else {
            self.kube.cleanup_run(&self.namespace, &self.run)
        };
        result.map_err(|_| {
            format!(
                "failed to delete guarded namespace/run {} {}; operator cleanup required",
                self.namespace.name(),
                self.run
            )
        })
    }
}

impl<K: NamespaceClient> Drop for NamespaceGuard<'_, K> {
    fn drop(&mut self) {
        if self.active
            && let Err(error) = self.cleanup()
        {
            eprintln!("{error}");
        }
    }
}
