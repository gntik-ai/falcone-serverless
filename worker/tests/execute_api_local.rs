//! The worker execution API over real sockets, in process. Guests run here only as the fast
//! feedback loop; `cluster_execute_api.rs` runs every security-relevant case again in a
//! restricted pod through the same cases (`execute_api_cases.rs`). Cases wait on server-side
//! events, never on a fixed sleep.

#[path = "../src/qualification/execute_api_support.rs"]
mod execute_api_support;

#[path = "../src/qualification/execute_api_cases.rs"]
mod cases;

use execute_api_support::*;
macro_rules! tests {
    ($($name:ident),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                cases::$name();
            }
        )*
        const REGISTERED: &[&str] = &[$(stringify!($name)),*];
    };
}

tests!(
    a_wasm_revision_returns_the_legacy_envelope_with_its_runtime_identity,
    a_projection_naming_another_engine_is_refused_before_anything_runs,
    another_runtime_mode_is_refused_and_no_instance_is_created,
    a_malformed_request_is_a_400_and_runs_nothing,
    an_oversized_action_body_is_refused_before_a_guest_runs,
    an_import_the_projection_does_not_declare_gets_422_before_guest_code_runs,
    an_ungated_falcone_interface_is_refused_unless_declared,
    a_declared_import_that_the_worker_does_not_provide_is_still_refused,
    a_component_whose_imports_are_all_declared_and_allowed_runs,
    a_one_mebibyte_memory_budget_fails_the_four_mebibyte_fixture_and_enough_passes,
    a_memory_budget_above_the_worker_ceiling_is_refused_before_compile,
    a_small_fuel_budget_stops_a_looping_guest_well_before_the_timeout,
    the_call_after_max_host_calls_is_refused_and_the_host_saw_exactly_n,
    with_every_slot_busy_the_body_is_not_read_and_the_answer_is_busy,
    with_every_slot_busy_a_non_wasm_request_with_its_body_buffered_still_gets_409,
    each_listener_refuses_the_other_listeners_operations,
    a_cancel_succeeds_with_all_slots_busy_and_the_execute_listener_full_of_stalled_connections,
    a_full_control_listener_does_not_touch_running_executions_and_recovers_after_its_deadlines,
    a_cancel_of_a_guest_blocked_in_a_mutating_host_call_is_uncertain,
    a_cancel_of_an_execution_that_is_not_running_is_uncertain,
    an_update_is_accepted_for_a_running_execution_and_refused_otherwise,
    a_request_deadline_shorter_than_the_projection_and_the_worker_limit_wins,
    a_projection_limit_shorter_than_the_request_and_the_worker_limit_wins,
    a_worker_wall_clock_limit_shorter_than_both_wins,
    setup_that_crosses_until_times_out_and_the_guest_never_starts,
    a_cancel_during_a_blocked_bind_ends_the_execution_and_frees_the_slot,
    a_host_call_blocking_near_expiry_ends_at_until_and_a_later_call_never_reaches_the_host,
    a_blocked_host_call_that_ends_before_until_leaves_the_budget_outcome_alone,
    a_cancel_during_cold_validation_frees_the_slot_within_the_termination_budget,
    a_short_timeout_during_cold_validation_times_out_within_the_termination_budget,
    a_cold_artifact_is_validated_once_per_execution,
    the_credential_is_redacted_from_the_logs_of_a_result,
    the_credential_is_absent_from_guest_memory,
    a_maximum_size_log_result_validates_and_is_bounded,
    oversized_headers_are_refused_without_running_a_guest,
    an_oversized_request_is_refused_before_its_body_is_read,
    an_oversized_control_message_is_refused_on_the_control_listener,
    an_idle_connection_is_closed_within_the_header_deadline,
    a_header_drip_is_closed_at_the_absolute_header_deadline,
    a_stalled_body_is_closed_at_the_absolute_body_deadline,
    a_slow_response_reader_is_cut_off_at_the_absolute_response_deadline,
    redaction_overlapping_secrets_leave_no_fragment_of_either,
    redaction_secrets_that_share_a_prefix_are_fully_redacted,
    redaction_a_secret_inside_another_is_covered_once,
    redaction_lines_without_secrets_and_logs_without_lines_pass_through,
    redaction_each_line_is_cut_before_redaction_and_the_output_is_cut_again,
    redaction_at_most_one_thousand_lines_are_returned,
    redaction_the_maximum_number_of_secrets_and_a_thousand_maximum_length_lines_finish_in_time,
    redaction_past_the_secret_budget_the_result_is_only_the_marker,
    redaction_past_the_match_budget_the_result_is_only_the_marker_and_quickly,
    host_the_handler_is_not_invoked_when_until_passes_after_instantiation,
    host_a_host_counts_every_entry_against_max_host_calls,
);

#[test]
fn every_case_is_also_registered_for_the_pod_runner() {
    let runner: Vec<&str> = cases::CASES.iter().map(|(name, _)| *name).collect();
    assert_eq!(runner, REGISTERED);
}

// --- the production binary --------------------------------------------------------------------

struct Child(std::process::Child, std::path::PathBuf);

impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        let _ = std::fs::remove_dir_all(&self.1);
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn the_worker_binary_serves_both_listeners_from_its_artifact_directory() {
    use std::io::BufRead;
    let directory = std::env::temp_dir().join(format!("serve-binary-{}", std::process::id()));
    std::fs::create_dir_all(directory.join("artifacts")).unwrap();
    let component = fixture("api-ok");
    let digest = digest(&component);
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    std::fs::write(directory.join(format!("artifacts/{hex}.wasm")), &component).unwrap();
    let (execute_port, control_port) = (free_port(), free_port());
    let config = format!(
        "pool_id = \"pool-local\"\nlisten_address = \"127.0.0.1\"\nlisten_port = {execute_port}\n\
         control_port = {control_port}\nmetrics_port = {}\nprobe_port = {}\n\
         cache_path = \"{}\"\nartifact_path = \"{}\"\n\
         falcone_api_endpoint = \"https://api.invalid\"\nfalcone_dispatcher_endpoint = \"https://dispatcher.invalid\"\n",
        free_port(),
        free_port(),
        directory.join("cache").display(),
        directory.join("artifacts").display(),
    );
    std::fs::write(directory.join("worker.toml"), config).unwrap();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_falcone-serverless-worker"))
        .arg("serve")
        .arg(directory.join("worker.toml"))
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let child = Child(child, directory);
    let listening: serde_json::Value = serde_json::from_str(&line).unwrap();
    let execute_address = listening["listening"]["execute"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let control_address = listening["listening"]["control"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let spec = Spec::new(digest);
    let answer = post(execute_address, "/v1/execute", &spec.bytes());
    assert_eq!(answer.status, 200);
    assert_eq!(answer.json()["identity"]["wasmtime_version"], "49.0.5");
    // An artifact the pod does not hold is unavailable, not a crash.
    let unknown = Spec::new([7; 32]);
    assert_eq!(
        post(execute_address, "/v1/execute", &unknown.bytes())
            .failure()
            .1,
        "artifact_unavailable"
    );
    // The listeners refuse each other's operations.
    assert_eq!(
        post(control_address, "/v1/execute", &spec.bytes()).status,
        404
    );
    assert_eq!(
        post(execute_address, "/v1/cancel", &spec.cancel()).status,
        404
    );
    assert_eq!(
        post(control_address, "/v1/cancel", &spec.cancel()).status,
        200
    );
    drop(child);
}
