pub use falcone_serverless_worker::{artifact, host, limits};
#[path = "../src/qualification/execute_cases.rs"]
mod cases;

#[test]
fn empty_handler_is_rejected_before_compilation() {
    let bytes = wat::parse_str(
        "(component (instance $h) (export \"falcone:host/handler@0.2.0\" (instance $h)))",
    )
    .unwrap();
    assert_eq!(
        artifact::validate::validate(&bytes, bytes.len())
            .unwrap_err()
            .code,
        "invalid_handler_signature"
    );
}

#[test]
fn utf16_is_rejected_before_compilation() {
    let source = include_str!("../src/qualification/execute_fixtures/handler.wat")
        .replace("string-encoding=utf8", "string-encoding=utf16");
    let bytes = wat::parse_str(source).unwrap();
    assert_eq!(
        artifact::validate::validate(&bytes, bytes.len())
            .unwrap_err()
            .code,
        "unsupported_string_encoding"
    );
}

#[test]
fn handler_binaries_match_independent_wat_oracles() {
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/qualification/execute_fixtures");
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|extension| extension == "wat") {
            let expected = wat::parse_file(&path).unwrap();
            assert!(
                expected == std::fs::read(path.with_extension("wasm")).unwrap(),
                "fixture differs: {}",
                path.display()
            );
        }
    }
}

#[test]
fn execute_command_echoes_params() {
    let root = std::env::temp_dir().join(format!("handler-cli-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let component = root.join("handler.wasm");
    let params = root.join("params.json");
    std::fs::write(
        &component,
        include_bytes!("../src/qualification/execute_fixtures/handler.wasm"),
    )
    .unwrap();
    std::fs::write(&params, b"{\"command\":true}").unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_falcone-serverless-worker"))
        .arg("execute")
        .arg(component)
        .arg(params)
        .output()
        .unwrap();
    std::fs::remove_dir_all(root).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"{\"command\":true}");
}

#[test]
fn output_limits_do_not_allocate_rejected_payloads() {
    for limit in [128, host::BUFFER_BYTES] {
        for kind in ["result", "error"] {
            let output =
                std::process::Command::new(env!("CARGO_BIN_EXE_compile-cache-qualification"))
                    .args(["--guest-execute-allocation", &limit.to_string(), kind])
                    .output()
                    .unwrap();
            assert!(
                output.status.success(),
                "{limit} {kind}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("guest_execute_allocation"));
        }
    }
}

#[test]
fn qualification_command_runs_the_selected_case() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_compile-cache-qualification"))
        .arg("--guest-execute=post-return")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("\"case\":\"post-return\""));
}
