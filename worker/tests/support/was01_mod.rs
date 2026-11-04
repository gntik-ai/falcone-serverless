//! The WAS-01 cluster harness: the modules `was01_cluster.rs` uses. It shares the image,
//! kubeconfig and namespace modules with the worker-image harness in `mod.rs`.

#[path = "../../src/qualification/cluster_evidence.rs"]
pub mod cluster_evidence;
#[path = "image.rs"]
mod image;
#[path = "kube_cluster.rs"]
pub mod kube_cluster;
#[path = "kubeconfig.rs"]
mod kubeconfig;
#[path = "namespace.rs"]
mod namespace;
#[path = "observation.rs"]
pub mod observation;
#[path = "render.rs"]
pub mod render;
#[path = "was01.rs"]
pub mod was01;
#[path = "was01_deps.rs"]
pub mod was01_deps;
#[path = "was01_plan.rs"]
pub mod was01_plan;
#[path = "was01_probe.rs"]
pub mod was01_probe;

pub use image::{final_uid, pinned_image};
pub use kubeconfig::Kubeconfigs;
pub use namespace::{
    Namespace, NamespaceApi, NamespaceMode, NamespacePlan, RUN_LABEL, namespace_name,
    test_namespace,
};
