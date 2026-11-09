//! Restricted-pod runner; each security criterion can be selected independently.
use crate::{execute_cases as cases, host};
use std::process::{Command, ExitCode};
use std::sync::Arc;

type Case = (&'static str, fn());
pub const CASES: &[Case] = &[
    ("echo", cases::echo),
    ("post-return", cases::output_survives_post_return_cleanup),
    (
        "cancel-before-deadline-success",
        cases::cancel_before_deadline_success,
    ),
    (
        "cancel-before-deadline-error",
        cases::cancel_before_deadline_error,
    ),
    ("output-limits", cases::configured_output_limit),
    ("params-limit", cases::oversized_params_never_call_guest),
    ("timeout-isolation", cases::timeout_isolation),
    ("cancel-success", cases::cancellation_on_success),
    ("cancel-error", cases::cancellation_on_error),
    ("deadline-success", cases::deadline_on_success),
    ("deadline-error", cases::deadline_on_error),
    ("expired-prepared", cases::expired_prepared_invocation),
    ("invalid-export", cases::invalid_export_before_host_call),
    ("wrong-signature", cases::wrong_signature_before_host_call),
    ("missing-export", cases::missing_export),
    ("forbidden-imports", cases::forbidden_imports),
    ("utf16", cases::utf16_before_host_call),
    ("latin1", cases::latin1_before_host_call),
    ("utf16-lower", cases::utf16_lower_before_host_call),
    ("uncertain-success", cases::uncertainty_on_success),
    ("uncertain-error", cases::uncertainty_on_error),
    ("uncertain-trap", cases::uncertainty_on_trap),
    ("uncertain-cancel", cases::uncertainty_on_cancellation),
    ("uncertain-limit", cases::uncertainty_on_size_limit),
    ("uncertain-deadline", cases::uncertainty_on_deadline),
    ("non-mutating-timeout", cases::non_mutating_timeout),
    ("allocation-small-result", allocation_small_result),
    ("allocation-small-error", allocation_small_error),
    ("allocation-default-result", allocation_default_result),
    ("allocation-default-error", allocation_default_error),
];

fn allocation(limit: usize, kind: &str) {
    assert!(
        Command::new(std::env::current_exe().unwrap())
            .args(["--guest-execute-allocation", &limit.to_string(), kind])
            .status()
            .unwrap()
            .success()
    );
}
fn allocation_small_result() {
    allocation(128, "result");
}
fn allocation_small_error() {
    allocation(128, "error");
}
fn allocation_default_result() {
    allocation(host::BUFFER_BYTES, "result");
}
fn allocation_default_error() {
    allocation(host::BUFFER_BYTES, "error");
}

pub fn run(name: &str) -> ExitCode {
    let Some((_, case)) = CASES.iter().find(|(candidate, _)| *candidate == name) else {
        eprintln!("unknown_guest_execute_case");
        return ExitCode::FAILURE;
    };
    case();
    println!("{}", serde_json::json!({"case":name,"status":"passed"}));
    println!(
        "{}",
        serde_json::json!({"check":"guest_execute","status":"passed"})
    );
    ExitCode::SUCCESS
}

struct Empty;
impl host::Bindings for Empty {
    fn call(&self, _: &host::Context, _: host::Operation) -> Result<host::Reply, host::HostError> {
        Err(host::HostError::Denied)
    }
}
pub fn allocation_main() -> ExitCode {
    let args: Vec<_> = std::env::args().collect();
    let limit: usize = args[2].parse().unwrap();
    assert!([128, host::BUFFER_BYTES].contains(&limit));
    let mode = match args[3].as_str() {
        "result" => 0,
        "error" => 2,
        _ => panic!("unknown allocation case"),
    };
    let engine = cases::engine();
    for size in [limit + 1, host::BUFFER_BYTES + 1] {
        let prepared = host::Prepared::new(
            &engine,
            cases::adversarial(),
            host::Host::with_limits(
                cases::context(),
                Arc::new(Empty),
                cases::limits(limit, 30000),
            ),
        )
        .unwrap();
        let params = cases::params(mode, size);
        let baseline = crate::counting_alloc::reset_peak();
        let result = prepared.call(&params);
        let growth = crate::counting_alloc::peak().saturating_sub(baseline);
        assert_eq!(result.result.unwrap_err().code, "buffer_limit_exceeded");
        // Allow fixed engine bookkeeping (including typed-error allocation).
        // A 1 MiB guest payload must stay below 4 KiB even with a 128 B limit.
        // The 129 B case independently pins the one-byte-over verdict.
        let bound = 4096;
        assert!(
            growth < bound,
            "rejected payload allocated {growth} bytes (bound {bound})"
        );
        let baseline = crate::counting_alloc::reset_peak();
        let control_size = size.max(2 * bound);
        let control = std::hint::black_box(vec![b'x'; control_size]);
        let growth = crate::counting_alloc::peak().saturating_sub(baseline);
        assert!(
            growth >= control_size && growth > bound,
            "allocator missed the eager negative control"
        );
        drop(control);
    }
    println!(
        "{}",
        serde_json::json!({"check":"guest_execute_allocation","status":"passed"})
    );
    ExitCode::SUCCESS
}
