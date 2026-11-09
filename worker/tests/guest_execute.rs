pub use falcone_serverless_worker::{artifact, cache, host, limits};
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
fn execute_command_refuses_execution_without_the_trusted_compiler() {
    let root = std::env::temp_dir().join(format!("handler-no-compiler-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let worker = root.join("falcone-serverless-worker");
    std::fs::copy(env!("CARGO_BIN_EXE_falcone-serverless-worker"), &worker).unwrap();
    let component = root.join("handler.wasm");
    let params = root.join("params.json");
    std::fs::write(
        &component,
        include_bytes!("../src/qualification/execute_fixtures/handler.wasm"),
    )
    .unwrap();
    std::fs::write(&params, b"must not execute").unwrap();
    let output = std::process::Command::new(worker)
        .arg("execute")
        .arg(component)
        .arg(params)
        .output()
        .unwrap();
    std::fs::remove_dir_all(root).unwrap();
    assert!(
        !output.status.success(),
        "execution bypassed the trusted compiler"
    );
    assert!(
        output.stdout.is_empty(),
        "rejected execution returned output"
    );
    assert_eq!(output.stderr, b"compile_failed\n");
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

#[test]
fn qualification_command_rejects_a_differently_named_export() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_compile-cache-qualification"))
        .arg("--guest-execute=different-export")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("\"case\":\"different-export\""));
}

#[test]
fn async_canonical_options_reject_non_utf8() {
    let base = include_str!("../src/qualification/execute_fixtures/handler.wat");
    let with_options = [
        "(core func (canon task.return (result string) (memory $guest \"memory\") ENCODING))",
        "(type $s (stream string)) (core func (canon stream.read $s (memory $guest \"memory\") (realloc (func $guest \"realloc\")) async ENCODING))",
        "(type $s (stream string)) (core func (canon stream.write $s (memory $guest \"memory\") async ENCODING))",
        "(type $f (future string)) (core func (canon future.read $f (memory $guest \"memory\") (realloc (func $guest \"realloc\")) async ENCODING))",
        "(type $f (future string)) (core func (canon future.write $f (memory $guest \"memory\") async ENCODING))",
    ];
    for canonical in with_options {
        for encoding in ["string-encoding=utf16", "string-encoding=latin1+utf16"] {
            let source = base.replacen(
                "(func $handle",
                &format!("{} (func $handle", canonical.replace("ENCODING", encoding)),
                1,
            );
            let bytes = wat::parse_str(source).unwrap();
            let Err(error) = artifact::validate::validate(&bytes, bytes.len()) else {
                panic!("non-UTF8 canonical accepted");
            };
            assert_eq!(
                error.code, "unsupported_string_encoding",
                "{canonical} {encoding}"
            );
        }
    }
}
