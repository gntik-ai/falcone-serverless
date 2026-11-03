//! The pieces of the shared cluster harness that the WAS-02 capability-gate test reuses: the
//! image pin check, the explicit kubeconfig boundary and the fs-test namespace guard.

#[path = "image.rs"]
mod image;
#[path = "kubeconfig.rs"]
mod kubeconfig;
#[path = "namespace.rs"]
mod namespace;

pub use image::{final_uid, pinned_image};
pub use kubeconfig::Kubeconfigs;
pub use namespace::{
    Namespace, NamespaceApi, NamespaceMode, NamespacePlan, RUN_LABEL, test_namespace,
};
