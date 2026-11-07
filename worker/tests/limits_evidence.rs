#[path = "../src/qualification/limits_evidence.rs"]
mod evidence;
#[path = "../src/qualification/outcome.rs"]
pub mod outcome;
use std::time::Duration;

#[test]
fn injected_success_missing_memory_event_excess_duration_and_skips_fail() {
    let status = |executed, observed, expected, elapsed| {
        serde_json::to_value(evidence::limits_fixture(
            executed,
            observed,
            expected,
            Duration::from_millis(elapsed),
            Some(Duration::from_millis(3000)),
            Ok(()),
        ))
        .unwrap()["status"]
            .clone()
    };
    assert_eq!(
        status(true, Some("guest-interrupted"), "guest-interrupted", 3000),
        "passed"
    );
    assert_eq!(
        status(true, Some("success"), "host-call-cancelled", 100),
        "failed"
    );
    assert_eq!(status(true, None, "guest-memory-limit", 1), "failed");
    assert_eq!(
        status(true, Some("guest-interrupted"), "guest-interrupted", 3001),
        "failed"
    );
    assert_eq!(
        status(false, Some("guest-interrupted"), "guest-interrupted", 1),
        "failed"
    );
}
