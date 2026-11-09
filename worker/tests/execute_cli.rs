#[test]
fn execute_image_installs_the_trusted_compiler_beside_the_worker() {
    let containerfile = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Containerfile"),
    )
    .unwrap();
    assert!(
        containerfile.lines().any(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            fields.first() == Some(&"COPY")
                && fields.contains(&"--from=build")
                && fields.ends_with(&[
                    "/build/target/release/artifact-compile-child",
                    "/usr/local/bin/artifact-compile-child",
                ])
        }),
        "execute requires the installed trusted compiler"
    );
}

#[test]
fn execute_requires_the_trusted_compiler_and_validates_before_spawning_it() {
    let scratch = std::env::temp_dir().join(format!("execute-no-compiler-{}", std::process::id()));
    std::fs::create_dir(&scratch).unwrap();
    let bin = scratch.join("worker");
    std::fs::copy(env!("CARGO_BIN_EXE_falcone-serverless-worker"), &bin).unwrap();
    let params = scratch.join("params");
    std::fs::write(&params, b"echo").unwrap();
    let invalid = scratch.join("invalid.wasm");
    std::fs::write(&invalid, wat::parse_str("(component)").unwrap()).unwrap();
    let echo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/qualification/execute_fixtures/echo.wasm");
    for (artifact, expected) in [
        (echo, "compile_failed"),
        (invalid, "missing_handler_export"),
    ] {
        let output = std::process::Command::new(&bin)
            .arg("execute")
            .arg(artifact)
            .arg(&params)
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "execution bypassed the trusted compiler"
        );
        assert!(output.stdout.is_empty());
        assert_eq!(String::from_utf8(output.stderr).unwrap().trim(), expected);
    }
    std::fs::remove_dir_all(scratch).unwrap();
}

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
