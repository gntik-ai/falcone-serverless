#[test]
fn execute_subcommand_echoes_params_and_rejects_invalid_exports_without_output() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!(
        "execute-cli-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&scratch).unwrap();
    let params = scratch.join("params.json");
    let invalid = scratch.join("invalid.wasm");
    std::fs::write(&params, br#"{"hello":"cli"}"#).unwrap();
    std::fs::write(&invalid, wat::parse_str("(component)").unwrap()).unwrap();
    let bin = env!("CARGO_BIN_EXE_falcone-serverless-worker");
    let output = std::process::Command::new(bin)
        .arg("execute")
        .arg(root.join("src/qualification/execute_fixtures/echo.wasm"))
        .arg(&params)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, br#"{"hello":"cli"}"#);
    let output = std::process::Command::new(bin)
        .arg("execute")
        .arg(&invalid)
        .arg(&params)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap().trim(),
        "missing_handler_export"
    );
    std::fs::remove_dir_all(scratch).unwrap();
}
