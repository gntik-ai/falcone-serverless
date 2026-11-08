use std::process::Command;

#[test]
fn termination_escalates_at_the_validated_budget_instead_of_killing_immediately() {
    for budget_ms in [80, 250] {
        let config = serde_json::json!({
            "version": 1, "compiler_cpu_seconds": 120,
            "compiler_memory_bytes": 4294967296_u64, "guest_memory_bytes": 2097152,
            "guest_to_host_bytes": 128, "host_to_guest_bytes": 128,
            "guest_deadline_ms": 100, "host_call_deadline_ms": 50,
            "termination_budget_ms": budget_ms
        });
        let start = std::time::Instant::now();
        let output = Command::new(env!("CARGO_BIN_EXE_compile-cache-qualification"))
            .args(["--termination-fallback", &config.to_string()])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(
            start.elapsed() >= std::time::Duration::from_millis(budget_ms),
            "supervisor returned before the configured grace period"
        );
        let records: Vec<serde_json::Value> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let evidence = records
            .iter()
            .find(|record| record["check"] == "termination_escalation")
            .expect("supervisor must report when it escalates after cooperative cancellation");
        assert_eq!(evidence["budget_ms"], budget_ms);
        let elapsed = evidence["elapsed_ms"].as_u64().unwrap();
        assert!(
            elapsed >= budget_ms,
            "killed before cooperative budget: {evidence}"
        );
        assert!(
            elapsed <= budget_ms + 2000,
            "termination deadline lost: {evidence}"
        );
    }
}

#[test]
fn qualification_termination_reaps_an_unresponsive_child_while_a_valid_guest_succeeds() {
    let output = Command::new(env!("CARGO_BIN_EXE_compile-cache-qualification"))
        .arg("--termination-fallback")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let records: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        records.iter().any(|record| record
            == &serde_json::json!({"check":"termination_fallback","status":"passed"}))
    );
}

#[test]
fn limits_runner_records_every_limit_and_cancellation_fixture() {
    let output = Command::new(env!("CARGO_BIN_EXE_compile-cache-qualification"))
        .arg("--limits-cancellation")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let records: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    for check in [
        "compiler_cpu",
        "compiler_memory",
        "guest_memory",
        "buffer_boundaries",
        "guest_deadline",
        "blocked_read",
        "binding_timeout",
        "mutation_cancellation",
        "http_completion_cancellation",
        "uncertain_interrupt",
        "uncertain_memory",
        "termination_fallback",
        "limits_cancellation",
    ] {
        assert!(
            records
                .iter()
                .any(|record| record == &serde_json::json!({"check":check,"status":"passed"})),
            "missing {check}: {records:?}"
        );
    }
}

#[test]
fn embedded_sources_match_the_committed_raw_guest_fixtures() {
    for name in ["buffers", "delayed-mutation", "guest-limits"] {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let source =
            std::fs::read(root.join(format!("src/qualification/limits_fixtures/{name}.wat")))
                .unwrap();
        assert_eq!(
            source,
            std::fs::read(root.join(format!("tests/fixtures/was-02/{name}.wat"))).unwrap()
        );
        wat::parse_bytes(&source).unwrap();
    }
}
