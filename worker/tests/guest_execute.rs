pub use falcone_serverless_worker::{artifact, cache, host, limits};
#[path = "../src/qualification/execute_cases.rs"]
mod cases;
#[path = "support/executable.rs"]
mod executable;

#[test]
fn completed_execution_retains_bounded_stdout_metadata() {
    use host::{Context, Host};
    use std::sync::Arc;
    struct Empty;
    impl host::Bindings for Empty {
        fn call(&self, _: &Context, _: host::Operation) -> Result<host::Reply, host::HostError> {
            panic!("stdout must not call platform bindings")
        }
    }
    let source = include_str!("fixtures/go/wasi-stdout.wat");
    let (prefix, _) = source.rsplit_once("  (func (export \"write\")").unwrap();
    let compiler = cases::compiler();
    for (length, trap, expected) in [
        (4, false, (4, false)),
        (65537, false, (65536, true)),
        (65537, true, (65536, true)),
    ] {
        let source = format!(
            r#"{prefix}
          (type $bytes (list u8))
          (type $handler-error (variant (case "invalid-input" string) (case "failed" string)))
          (type $result (result $bytes (error $handler-error)))
          (core module $handler
            (import "host" "memory" (memory 2))
            (import "host" "write" (func $write (param i32 i32) (result i64)))
            (export "memory" (memory 0))
            (func (export "realloc") (param i32 i32 i32 i32) (result i32) i32.const 70000)
            (func (export "handle") (param i32 i32) (result i32)
              i32.const {length} i32.const 1 call $write drop
              {failure}
              i32.const 0 i64.const 0 i64.store
              i32.const 8 i32.const 0 i32.store
              i32.const 0))
          (core instance $handler (instantiate $handler (with "host" (instance
            (export "memory" (memory $mem "memory"))
            (export "write" (func $guest "write"))))))
          (func $handle (param "params" $bytes) (result $result)
            (canon lift (core func $handler "handle") (memory $handler "memory")
              (realloc (func $handler "realloc"))))
          (instance $export (export "handler-error" (type $handler-error)) (export "handle" (func $handle)))
          (export "falcone:host/handler@0.2.0" (instance $export)))"#,
            failure = if trap { "unreachable" } else { "" }
        );
        let bytes = wat::parse_str(source).unwrap();
        let execution = cases::prepare(
            &compiler,
            &bytes,
            Host::new(Context::default(), Arc::new(Empty)),
        )
        .unwrap()
        .call(b"");
        assert_eq!(execution.result.is_err(), trap, "{execution:?}");
        assert_eq!(
            (execution.stdout_bytes, execution.stdout_truncated),
            expected
        );
    }
}

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
    executable::copy(
        std::path::Path::new(env!("CARGO_BIN_EXE_falcone-serverless-worker")),
        &worker,
    )
    .unwrap();
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
