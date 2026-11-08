//! In-process runs of the guest execute cases. The same cases run in the restricted
//! cluster pod through `compile-cache-qualification --guest-execute`.
pub use falcone_serverless_worker::{host, limits};
#[path = "../src/qualification/guest_execute_cases.rs"]
pub mod cases;
#[path = "../src/qualification/handler_fixtures.rs"]
pub mod handler_fixtures;

/// The runner iterates `CASES`, so these names are exactly what the cluster pod runs and
/// reports; the cluster test requires each of them (plus the trusted compile path).
#[test]
fn the_runner_table_holds_one_distinct_case_per_security_criterion() {
    let names: Vec<&str> = cases::CASES.iter().map(|(name, _)| *name).collect();
    for required in [
        "successful_handler_call",
        "size_ceilings",
        "limits_are_configurable",
        "timeout_isolation",
        "cancellation_precedence",
        "deadline_precedence",
        "host_call_timeout_is_not_cancellation",
        "oversized_guest_data",
        "bounded_errors",
        "signature_rejection",
    ] {
        assert_eq!(names.iter().filter(|name| **name == required).count(), 1);
    }
    assert_eq!(names.len(), 10);
}
