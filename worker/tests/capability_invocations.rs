pub use falcone_serverless_worker::host;
#[path = "../src/qualification/capability_support.rs"]
mod capability_support;

#[path = "../src/qualification/counting_alloc.rs"]
mod counting_alloc;

#[path = "../src/qualification/capability_invocations.rs"]
pub mod qualification;
