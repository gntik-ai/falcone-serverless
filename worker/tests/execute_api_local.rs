//! RUN-01.t3 local qualification of the worker execution API.
//!
//! Each case starts the real `falcone-serverless-worker serve` binary and speaks HTTP to its
//! execute and control listeners over loopback TCP; the same cases run in the restricted pod
//! (`cluster_execute_api.rs`). Nothing here uses a credential: requests carry synthetic markers.

#[path = "../src/qualification/execution_api.rs"]
mod execution_api;

fn worker() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_falcone-serverless-worker"))
}

macro_rules! cases {
    ($($name:ident => $case:literal),* $(,)?) => {$(
        #[test]
        fn $name() {
            if let Err(reason) = execution_api::run($case, &worker()) {
                panic!("{}: {reason}", $case);
            }
        }
    )*};
}

cases! {
    legacy_envelope_carries_the_runtime_identity => "envelope",
    runtime_mode_is_refused_first_and_refusals_pass_through => "runtime-mode",
    credential_is_absent_from_the_guest_and_redacted_from_logs => "credential",
    imports_must_be_declared_and_allowed => "imports",
    redaction_under_load_is_bounded_and_releases_the_slot => "redaction-load",
    saturated_execute_listener_still_cancels_and_answers_busy => "saturation",
    stalled_control_connections_leave_executions_unaffected => "control-stall",
    each_listener_refuses_the_others_operations => "cross-listener",
    oversized_headers_are_refused => "headers",
    oversized_requests_are_refused => "request",
    oversized_action_bodies_are_refused => "action-body",
    silent_connections_are_closed => "silent",
    dripping_headers_are_closed => "drip",
    stalled_bodies_are_closed => "stalled-body",
}

#[test]
fn every_case_has_a_local_test() {
    let source = include_str!("execute_api_local.rs");
    for (name, _) in execution_api::CASES {
        assert!(source.contains(&format!("=> \"{name}\"")), "{name}");
    }
}
