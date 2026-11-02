use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Label carried by every object a run creates in a shared namespace.
pub const RUN_LABEL: &str = "falcone.test/run";
const ENFORCE_LABEL: &str = "pod-security.kubernetes.io/enforce";

fn random_hex() -> (String, u64) {
    use std::collections::hash_map::RandomState;
    use std::hash::BuildHasher;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let counter = NEXT.fetch_add(1, Ordering::Relaxed);
    let random = RandomState::new().hash_one((std::process::id(), counter));
    (format!("{random:016x}"), counter)
}

/// `fs-test-<suite>-<random>`; the suite is a short DNS-1123 fragment such as `was01`.
pub fn namespace_name(suite: &str) -> String {
    let (random, counter) = random_hex();
    format!("fs-test-{suite}-{random}-{counter:x}")
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
            "labels": {ENFORCE_LABEL: "restricted"}}})
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum NamespaceMode {
    Create,
    Shared(Namespace),
}

impl NamespaceMode {
    pub fn parse(mode: Option<&str>, namespace: Option<&str>) -> Result<Self, String> {
        match mode {
            None | Some("create") => Ok(Self::Create),
            Some("shared") => {
                let namespace = namespace.filter(|name| !name.trim().is_empty()).ok_or(
                    "UNVERIFIED: FS_TEST_NAMESPACE is required for FS_TEST_NAMESPACE_MODE=shared",
                )?;
                Ok(Self::Shared(Namespace::new(namespace).map_err(|error| {
                    format!("UNVERIFIED: FS_TEST_NAMESPACE: {error}")
                })?))
            }
            _ => Err("FS_TEST_NAMESPACE_MODE must be create or shared".into()),
        }
    }
}

/// The cluster operations behind a test namespace, always with the test-admin identity.
pub trait NamespaceApi {
    /// SelfSubjectAccessReview: may this identity create namespaces?
    fn can_create_namespaces(&self) -> Result<bool, String>;
    fn create_namespace(&self, namespace: &Namespace) -> Result<(), String>;
    /// Labels of an existing namespace, or `None` when it does not exist.
    fn namespace_labels(
        &self,
        namespace: &Namespace,
    ) -> Result<Option<BTreeMap<String, String>>, String>;
    fn delete_namespace(&self, namespace: &Namespace) -> Result<(), String>;
    /// Deletes the namespaced objects labelled `falcone.test/run=<run>`, and nothing else.
    fn delete_run_objects(&self, namespace: &Namespace, run: &str) -> Result<(), String>;
}

/// The namespace choice made without any client call, so manifests can be rendered and
/// refused before the first one.
#[derive(Debug)]
pub struct NamespacePlan {
    namespace: Namespace,
    run: String,
    owned: bool,
}

impl NamespacePlan {
    pub fn new(suite: &str, mode: &NamespaceMode) -> Result<Self, String> {
        let mut plan = Self::for_run(&namespace_name(suite), mode)?;
        // Short enough to suffix object names within DNS-1123 limits.
        plan.run = random_hex().0;
        Ok(plan)
    }

    /// Like `new`, with the run (and, in create mode, the namespace) named by the caller.
    pub fn for_run(name: &str, mode: &NamespaceMode) -> Result<Self, String> {
        let (namespace, owned) = match mode {
            NamespaceMode::Create => (Namespace::new(name)?, true),
            NamespaceMode::Shared(namespace) => (namespace.clone(), false),
        };
        Ok(Self {
            namespace,
            run: name.to_owned(),
            owned,
        })
    }

    pub fn namespace(&self) -> &Namespace {
        &self.namespace
    }

    pub fn run(&self) -> &str {
        &self.run
    }

    /// Create mode: permission check, then create. Shared mode: the namespace must exist and
    /// carry enforce=restricted. Both fail before any mutation of an object.
    pub fn acquire<'a, K: NamespaceApi + ?Sized>(
        self,
        kube: &'a K,
    ) -> Result<TestNamespace<'a, K>, String> {
        if self.owned {
            // A failed review is as unverified as a denied one; neither mutates anything.
            if !kube.can_create_namespaces().unwrap_or(false) {
                return Err("UNVERIFIED: the test identity may not create namespaces; \
                    set FS_TEST_NAMESPACE_MODE=shared and FS_TEST_NAMESPACE to use an existing fs-test-* namespace"
                    .into());
            }
            kube.create_namespace(&self.namespace).map_err(|_| {
                "FS_TEST_NAMESPACE_MODE=create: namespace creation denied or unavailable"
                    .to_string()
            })?;
        } else {
            let labels = kube
                .namespace_labels(&self.namespace)
                .map_err(|_| "UNVERIFIED: FS_TEST_NAMESPACE cannot be read".to_string())?
                .ok_or("UNVERIFIED: FS_TEST_NAMESPACE does not exist")?;
            if labels.get(ENFORCE_LABEL).map(String::as_str) != Some("restricted") {
                return Err(
                    "UNVERIFIED: FS_TEST_NAMESPACE is not labelled pod-security.kubernetes.io/enforce=restricted"
                        .into(),
                );
            }
        }
        Ok(TestNamespace {
            kube,
            plan: self,
            done: false,
        })
    }
}

/// A namespace for one test run and its cleanup guard. Dropping it cleans up on success,
/// failure and panic: create mode deletes the namespace, shared mode deletes only the objects
/// labelled with this run.
pub struct TestNamespace<'a, K: NamespaceApi + ?Sized> {
    kube: &'a K,
    plan: NamespacePlan,
    done: bool,
}

impl<K: NamespaceApi + ?Sized> TestNamespace<'_, K> {
    pub fn namespace(&self) -> &Namespace {
        &self.plan.namespace
    }

    pub fn run(&self) -> &str {
        &self.plan.run
    }

    /// Reports a cleanup failure instead of only logging it; cleanup runs at most once.
    pub fn close(mut self) -> Result<(), String> {
        self.cleanup()
    }

    fn cleanup(&mut self) -> Result<(), String> {
        if std::mem::replace(&mut self.done, true) {
            return Ok(());
        }
        let result = if self.plan.owned {
            self.kube.delete_namespace(&self.plan.namespace)
        } else {
            self.kube
                .delete_run_objects(&self.plan.namespace, &self.plan.run)
        };
        result.map_err(|_| {
            format!(
                "failed to clean up {}; operator cleanup required (run {})",
                self.plan.namespace.name(),
                self.plan.run
            )
        })
    }
}

impl<K: NamespaceApi + ?Sized> Drop for TestNamespace<'_, K> {
    fn drop(&mut self) {
        if let Err(message) = self.cleanup() {
            eprintln!("{message}");
        }
    }
}

/// The shared entry point of every falcone-serverless cluster test: the namespace for
/// `suite` (a short DNS-1123 fragment) according to the namespace mode, and its guard.
pub fn test_namespace<'a, K: NamespaceApi + ?Sized>(
    suite: &str,
    kube: &'a K,
    mode: &NamespaceMode,
) -> Result<TestNamespace<'a, K>, String> {
    NamespacePlan::new(suite, mode)?.acquire(kube)
}
