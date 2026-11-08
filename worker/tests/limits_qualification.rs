use std::process::Command;

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
        "mutation_cancellation",
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
