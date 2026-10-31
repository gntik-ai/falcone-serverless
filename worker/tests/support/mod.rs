//! Test-only worker image qualification; no runtime code or credential handling.

mod cleanup;
pub mod cli;
mod config;
mod namespace;
mod observation;
mod pod;
mod session;

pub use config::Config;
pub use namespace::{
    Namespace, NamespaceClient, NamespaceGuard, NamespaceMode, namespace_name, test_namespace,
};
pub use pod::{choose_uid, deployment, final_uid, pod_spec};
use pod::{pinned_image, validate_spec};
pub use session::{Kube, Session};

pub mod live;
pub mod was01;
