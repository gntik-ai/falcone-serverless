//! Test-only worker image qualification; no runtime code or credential handling.

pub mod cli;
mod config;
mod image;
mod kubeconfig;
mod namespace;
mod pod;
mod session;

pub use config::Config;
pub use image::{final_uid, pinned_image};
pub use kubeconfig::Kubeconfigs;
pub use namespace::{
    Namespace, NamespaceApi, NamespaceMode, NamespacePlan, RUN_LABEL, TestNamespace,
    namespace_name, test_namespace,
};
pub use pod::{deployment, pod_spec};
use pod::validate_spec;
pub use session::{Kube, Session};
