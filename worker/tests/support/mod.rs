//! Test-only worker image qualification; no runtime code or credential handling.

pub mod cli;
mod config;
mod namespace;
mod pod;
mod session;

pub use config::Config;
pub use namespace::{Namespace, NamespaceMode, namespace_name};
pub use pod::{deployment, final_uid, pod_spec};
use pod::{pinned_image, validate_spec};
pub use session::{Kube, Session};
