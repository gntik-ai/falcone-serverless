//! In-process runs of the guest execute cases. The same cases run in the restricted
//! cluster pod through `compile-cache-qualification --guest-execute`.
pub use falcone_serverless_worker::{host, limits};
#[path = "../src/qualification/guest_execute_cases.rs"]
pub mod cases;
#[path = "../src/qualification/handler_fixtures.rs"]
pub mod handler_fixtures;
