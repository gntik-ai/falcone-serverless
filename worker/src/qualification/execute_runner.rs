//! Restricted-pod qualification shares the public-API unit cases, including
//! fresh child processes for allocation measurements (ADR 0014 link scope).
pub fn main() -> std::process::ExitCode {
    use crate::execute_cases::*;
    let cases: &[(&str, fn())] = &[
        (
            "successful_handler_call",
            successful_handler_echoes_the_params,
        ),
        ("host_capability", version_two_host_capability_is_callable),
        (
            "signature_before_host_call",
            handler_rejections_precede_guest_start_and_host_calls,
        ),
        (
            "utf16_before_host_call",
            utf16_rejection_precedes_guest_start_and_host_calls,
        ),
        (
            "latin1_before_host_call",
            latin1_rejection_precedes_guest_start_and_host_calls,
        ),
        (
            "host_to_guest_limits",
            params_are_bounded_before_guest_calls_at_small_and_default_limits,
        ),
        (
            "guest_to_host_limits",
            utf8_result_and_error_payloads_obey_configured_limits,
        ),
        (
            "guest_to_host_allocations",
            oversized_results_and_errors_are_rejected_before_host_allocation,
        ),
        (
            "timeout_isolation",
            guest_deadline_interrupts_only_its_store_on_a_shared_engine,
        ),
        (
            "blocked_host_call_deadline",
            guest_deadline_also_bounds_a_blocked_host_call,
        ),
        ("cancelled_success", cancellation_precedes_success),
        ("cancelled_error", cancellation_precedes_handler_error),
        ("expired_success", deadline_precedes_success),
        ("expired_error", deadline_precedes_handler_error),
    ];
    crate::capability_invocations::allocation_instrumentation_reports_an_allocation_above_the_bound(
    );
    for (name, case) in cases {
        case();
        println!("{}", serde_json::json!({"case":name,"status":"passed"}));
    }
    println!(
        "{}",
        serde_json::json!({"check":"handler_execution","status":"passed"})
    );
    std::process::ExitCode::SUCCESS
}
