//! The worker's `execute` subcommand, black box: validate, trusted compile in the child
//! process, then one `handle` call. Success writes exactly the result bytes to stdout;
//! every failure writes nothing to stdout and one bounded `execute failed: <code>` line.
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[path = "../src/qualification/handler_fixtures.rs"]
pub mod handler_fixtures;

struct Scratch(PathBuf);
impl Scratch {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("worker-execute-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn worker(args: &[&Path]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_falcone-serverless-worker"))
        .arg("execute")
        .args(args)
        .output()
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

fn failed(output: &Output) -> String {
    assert_eq!(output.status.code(), Some(1), "{}", stderr(output));
    assert!(
        output.stdout.is_empty(),
        "a failed invocation must write no stdout"
    );
    let message = stderr(output);
    assert_eq!(message.lines().count(), 1, "{message}");
    assert!(
        message.len() <= 512,
        "diagnostic must stay bounded: {message}"
    );
    message.trim_end().to_owned()
}

#[test]
fn success_writes_exactly_the_result_bytes_and_exits_zero() {
    let scratch = Scratch::new("ok");
    let component = scratch.file("echo.wasm", handler_fixtures::ECHO);
    let params: Vec<u8> = (0..=255).collect();
    let output = worker(&[&component, &scratch.file("params.bin", &params)]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(output.stdout, params);
    assert!(output.stderr.is_empty());
}

#[test]
fn oversized_params_fail_with_the_limit_code_and_no_stdout() {
    let scratch = Scratch::new("params");
    let component = scratch.file("echo.wasm", handler_fixtures::ECHO);
    let exact = scratch.file("exact.bin", &vec![9; 1_048_576]);
    let output = worker(&[&component, &exact]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(output.stdout.len(), 1_048_576);
    let over = scratch.file("over.bin", &vec![9; 1_048_577]);
    assert_eq!(
        failed(&worker(&[&component, &over])),
        "execute failed: buffer_limit_exceeded"
    );
}

#[test]
fn an_oversized_result_fails_with_the_limit_code_and_no_stdout() {
    let scratch = Scratch::new("result");
    let component = scratch.file("modes.wasm", handler_fixtures::MODES);
    let sized = |bytes: u32| [&b"s"[..], &bytes.to_le_bytes()].concat();
    let exact = scratch.file("exact.bin", &sized(1_048_576));
    let output = worker(&[&component, &exact]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(output.stdout.len(), 1_048_576);
    let over = scratch.file("over.bin", &sized(1_048_577));
    assert_eq!(
        failed(&worker(&[&component, &over])),
        "execute failed: buffer_limit_exceeded"
    );
}

#[test]
fn a_component_the_gate_rejects_never_runs() {
    let scratch = Scratch::new("gate");
    let params = scratch.file("params.bin", b"{}");
    for (wasm, expected) in [
        (
            handler_fixtures::START_LOGS_WITHOUT_HANDLE,
            "execute failed: invalid_handler_signature",
        ),
        (
            handler_fixtures::WRONG_RESULT,
            "execute failed: invalid_handler_signature",
        ),
        (
            b"\0asm\x0d\0\x01\0".as_slice(),
            "execute failed: missing_handler_export",
        ),
        (
            b"\0asm\x0d\0\x01\0\xff".as_slice(),
            "execute failed: invalid_module",
        ),
        (
            b"\x7fELF\x02\x01\x01\0".as_slice(),
            "execute failed: precompiled_rejected",
        ),
    ] {
        let component = scratch.file("component.wasm", wasm);
        let message = failed(&worker(&[&component, &params]));
        assert!(message.starts_with(expected), "{message}");
    }
}

#[test]
fn handler_errors_and_traps_are_bounded_failures() {
    let scratch = Scratch::new("errors");
    let component = scratch.file("modes.wasm", handler_fixtures::MODES);
    for (mode, expected) in [
        (&b"f"[..], "execute failed: handler_failed: boom"),
        (b"i", "execute failed: invalid_input: bad input"),
        (b"t", "execute failed: guest_trapped"),
    ] {
        let params = scratch.file("params.bin", mode);
        assert_eq!(failed(&worker(&[&component, &params])), expected);
    }
    // A 1000-character guest message is cut to 256 characters.
    let params = scratch.file("params.bin", b"g");
    let message = failed(&worker(&[&component, &params]));
    assert_eq!(
        message,
        format!("execute failed: handler_failed: {}", "x".repeat(256))
    );
}

#[test]
fn usage_and_unreadable_inputs_are_failures_with_no_stdout() {
    let scratch = Scratch::new("usage");
    let params = scratch.file("params.bin", b"{}");
    assert!(failed(&worker(&[])).starts_with("usage: "));
    assert!(failed(&worker(&[&params])).starts_with("usage: "));
    let missing = scratch.0.join("missing.wasm");
    assert!(failed(&worker(&[&missing, &params])).starts_with("execute failed: read component"));
    let component = scratch.file("echo.wasm", handler_fixtures::ECHO);
    assert!(failed(&worker(&[&component, &missing])).starts_with("execute failed: read params"));
    let version = Command::new(env!("CARGO_BIN_EXE_falcone-serverless-worker"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(version.status.success());
}
