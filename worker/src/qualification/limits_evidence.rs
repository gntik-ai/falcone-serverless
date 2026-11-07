use super::outcome::{Outcome, operation};

/// A fixture must execute, observe its specified event and stay in budget.
/// A handled guest error or missing observation cannot be promoted to success.
pub fn limits_fixture(
    executed: bool,
    observed: Option<&str>,
    expected: &str,
    elapsed: std::time::Duration,
    maximum: Option<std::time::Duration>,
    result: Result<(), String>,
) -> Outcome {
    if !executed {
        return Outcome::failed("fixture did not execute", "limits-cancellation");
    }
    if let Err(reason) = result {
        return Outcome::failed(reason, "limits-cancellation");
    }
    if observed != Some(expected) {
        return Outcome::failed(
            format!("expected {expected}, observed {observed:?}"),
            "limits-cancellation",
        );
    }
    if maximum.is_some_and(|maximum| elapsed > maximum) {
        return Outcome::failed(
            "fixture exceeded deadline plus tolerance",
            "limits-cancellation",
        );
    }
    operation(Ok(()), "limits-cancellation")
}
