#![cfg(target_os = "linux")]
use serde_json::Value;
use std::process::Command;

#[test]
fn runner_executes_every_fixture_and_reaps_the_cancel_ignoring_child() {
    let output = Command::new(env!("CARGO_BIN_EXE_compile-cache-qualification"))
        .arg("--limits-cancellation")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let records: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let expected = [
        "compile_cpu",
        "compile_memory",
        "guest_memory",
        "guest_to_host",
        "storage_reply",
        "http_reply",
        "guest_deadline",
        "host_call_deadline",
        "termination",
    ];
    assert_eq!(records.len(), expected.len());
    let outcomes = [
        "compile_cpu_limit_exceeded",
        "compile_memory_limit_exceeded",
        "guest-memory-limit",
        "buffer-limit",
        "buffer-limit",
        "buffer-limit",
        "guest-interrupted",
        "host-call-cancelled",
        "terminated",
    ];
    for (name, outcome) in expected.into_iter().zip(outcomes) {
        let matching: Vec<_> = records
            .iter()
            .filter(|record| record["check"] == name)
            .collect();
        assert_eq!(matching.len(), 1, "{name}: {records:?}");
        assert_eq!(matching[0]["status"], "passed", "{name}: {records:?}");
        assert_eq!(
            matching[0]["evidence"]["outcome"], outcome,
            "{name}: {records:?}"
        );
    }
    let termination = records
        .iter()
        .find(|record| record["check"] == "termination")
        .unwrap();
    assert_eq!(termination["evidence"]["outcome"], "terminated");
    assert_eq!(termination["evidence"]["signal"], 9);
    assert_eq!(termination["evidence"]["reaped"], true);
    assert_eq!(termination["evidence"]["overlapping_valid_guest"], true);
    assert!(
        termination["evidence"]["cancel_elapsed_ms"]
            .as_u64()
            .unwrap()
            <= 7000
    );
}

#[test]
fn unknown_runner_mode_exits_nonzero_without_a_success_record() {
    let output = Command::new(env!("CARGO_BIN_EXE_compile-cache-qualification"))
        .arg("--unknown-limits-mode")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("passed"));
}
