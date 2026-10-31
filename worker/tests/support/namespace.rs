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
                let namespace = namespace
                    .ok_or("FS_TEST_NAMESPACE is required for FS_TEST_NAMESPACE_MODE=shared")?;
                Ok(Self::Shared(Namespace::new(namespace)?))
            }
            _ => Err("FS_TEST_NAMESPACE_MODE must be create or shared".into()),
        }
    }
}
