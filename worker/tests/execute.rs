#![cfg(target_os = "linux")]
//! The execute path, driven only through `host::execute` and the worker binary.
//! Guest components are written inline as WAT: each test names the behaviour it needs.

use falcone_serverless_worker::host::execute::{self, Executor, Limits};
use falcone_serverless_worker::host::{Bindings, Context, Host, HostError, Operation, Reply};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use wasmtime::component::Component;

#[path = "../src/qualification/execute_fixtures.rs"]
mod execute_fixtures;

const MIB: usize = 1_048_576;

/// A guest whose `handle` body is supplied by the test. The core module sees `$ptr` and
/// `$len` (the params) and returns a pointer to a `result<list<u8>, handler-error>`:
/// a tag word, then either (ptr, len) or (variant tag, string ptr, string len).
fn handler(imports: &str, host_funcs: &str, handle_body: &str) -> String {
    format!(
        r#"(component
  {imports}
  (core module $base
    (memory (export "memory") 20)
    (global $heap (mut i32) (i32.const 4096))
    (func (export "cabi_realloc") (param $old i32) (param $old_size i32) (param $align i32) (param $size i32) (result i32)
      (local $ptr i32)
      (local.set $ptr
        (i32.and
          (i32.add (global.get $heap) (i32.sub (local.get $align) (i32.const 1)))
          (i32.sub (i32.const 0) (local.get $align))))
      (global.set $heap (i32.add (local.get $ptr) (local.get $size)))
      (local.get $ptr)))
  (core instance $base (instantiate $base))
  (alias core export $base "memory" (core memory $memory))
  {host_funcs}
  (core module $guest
    (import "base" "memory" (memory 20))
    {guest_imports}
    (data (i32.const 1024) "boomping")
    (func (export "handle") (param $ptr i32) (param $len i32) (result i32)
      {handle_body}))
  (core instance $guest (instantiate $guest (with "base" (instance $base)) {guest_with}))
  (type $error-def (variant (case "invalid-input" string) (case "failed" string)))
  (instance $types (export "handler-error" (type $error-def)))
  (alias export $types "handler-error" (type $error))
  (type $outcome (result (list u8) (error $error)))
  (func $handle (param "params" (list u8)) (result $outcome)
    (canon lift (core func $guest "handle") (memory $base "memory") (realloc (func $base "cabi_realloc"))))
  (instance $handler (export "handler-error" (type $error)) (export "handle" (func $handle)))
  (export "falcone:host/handler@0.2.0" (instance $handler)))"#,
        guest_imports = if imports.is_empty() {
            ""
        } else {
            r#"(import "host" "log" (func $log (param i32 i32 i32 i32 i32 i32)))"#
        },
        guest_with = if imports.is_empty() {
            ""
        } else {
            r#"(with "host" (instance $host))"#
        },
    )
}

const LOGGING_IMPORT: &str = r#"(import "falcone:host/logging@0.2.0" (instance $logging
    (type $error-type (variant (case "denied") (case "limit-exceeded") (case "timeout") (case "cancelled")))
    (export "host-error" (type $error (eq $error-type)))
    (type $level-type (enum "debug" "info" "warn" "error"))
    (export "level" (type $level (eq $level-type)))
    (type $field-type (record (field "name" string) (field "value" string)))
    (export "field" (type $field (eq $field-type)))
    (export "log" (func (param "severity" $level) (param "message" string) (param "fields" (list $field)) (result (result (error $error)))))))"#;

const LOGGING_FUNCS: &str = r#"(alias export $logging "log" (func $log))
  (core func $log (canon lower (func $log) (memory $memory)))
  (core instance $host (export "log" (func $log)))"#;

/// Returns the params unchanged: `Ok(list)` with the same pointer and length.
const ECHO: &str = r#"(i32.store (i32.const 0) (i32.const 0))
      (i32.store (i32.const 4) (local.get $ptr))
      (i32.store (i32.const 8) (local.get $len))
      (i32.const 0)"#;

/// `Err(<variant case>("boom"))`: the string is a data segment at 1024.
fn fail_with(case: u32) -> String {
    format!(
        r#"(i32.store (i32.const 0) (i32.const 1))
      (i32.store8 (i32.const 4) (i32.const {case}))
      (i32.store (i32.const 8) (i32.const 1024))
      (i32.store (i32.const 12) (i32.const 4))
      (i32.const 0)"#
    )
}

fn echo() -> String {
    handler("", "", ECHO)
}

/// Never returns, and makes no host call.
fn spin() -> String {
    handler("", "", "(loop $forever (br $forever)) (i32.const 0)")
}

/// Calls `logging.log` once, then echoes (params[0] == 0) or fails with `failed`.
fn log_then_echo_or_fail() -> String {
    handler(
        LOGGING_IMPORT,
        LOGGING_FUNCS,
        &format!(
            r#"(call $log (i32.const 1) (i32.const 1028) (i32.const 4) (i32.const 0) (i32.const 0) (i32.const 64))
      (if (i32.load8_u (local.get $ptr)) (then
        {}
        (return)))
      {ECHO}"#,
            fail_with(1)
        ),
    )
}

#[derive(Default)]
struct Counting {
    calls: AtomicUsize,
    /// What a call does once counted: nothing, block, or raise the cancel flag.
    block: Duration,
    cancel: std::sync::Mutex<Option<Arc<AtomicBool>>>,
}

impl Bindings for Counting {
    fn call(&self, _: &Context, operation: Operation) -> Result<Reply, HostError> {
        assert!(matches!(operation, Operation::Log { .. }));
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(flag) = self.cancel.lock().unwrap().as_ref() {
            flag.store(true, Ordering::Release);
        }
        std::thread::sleep(self.block);
        Ok(Reply::Empty)
    }
}

fn host(bindings: &Arc<Counting>) -> Host {
    Host::new(Context::default(), bindings.clone())
}

fn limits(deadline: Duration) -> Limits {
    Limits {
        guest_deadline: deadline,
    }
}

fn executor() -> Executor {
    Executor::new(&execute::engine().unwrap()).unwrap()
}

fn run(wat: &str, params: &[u8]) -> Result<Vec<u8>, &'static str> {
    let bindings = Arc::new(Counting::default());
    executor()
        .execute_source(
            &wat::parse_str(wat).unwrap(),
            host(&bindings),
            params,
            limits(Duration::from_secs(20)),
        )
        .map_err(|error| error.code)
}

#[test]
fn echo_returns_params_byte_for_byte() {
    let all_bytes: Vec<u8> = (0..=255).cycle().take(3000).collect();
    for params in [&b""[..], b"{\"a\":1}", &all_bytes, &vec![7; MIB]] {
        assert_eq!(run(&echo(), params).unwrap(), params);
    }
}

#[test]
fn params_over_the_host_to_guest_ceiling_fail_with_the_limit_code() {
    assert_eq!(run(&echo(), &vec![0; MIB + 1]), Err("limit_exceeded"));
}

#[test]
fn result_over_the_guest_to_host_ceiling_fails_with_the_limit_code() {
    // The echo of a 1 MiB + 1 buffer cannot be produced from a valid input, so the guest
    // builds an oversized list itself: tag Ok, pointer 2048, length 1 MiB + 1.
    let oversized = handler(
        "",
        "",
        r#"(i32.store (i32.const 0) (i32.const 0))
      (i32.store (i32.const 4) (i32.const 2048))
      (i32.store (i32.const 8) (i32.const 1048577))
      (i32.const 0)"#,
    );
    assert_eq!(run(&oversized, b"{}"), Err("limit_exceeded"));
    // The ceiling itself is accepted.
    let exact = oversized.replace("1048577", "1048576");
    assert_eq!(run(&exact, b"{}").unwrap().len(), MIB);
}

#[test]
fn handler_errors_and_traps_map_to_bounded_codes_without_the_message() {
    assert_eq!(
        run(&handler("", "", &fail_with(0)), b"{}"),
        Err("invalid_input")
    );
    assert_eq!(
        run(&handler("", "", &fail_with(1)), b"{}"),
        Err("handler_failed")
    );
    assert_eq!(run(&handler("", "", "unreachable"), b"{}"), Err("trap"));
    // The error type carries only a stable code: the guest's text cannot leak through it.
    let bindings = Arc::new(Counting::default());
    let error = executor()
        .execute_source(
            &wat::parse_str(handler("", "", &fail_with(1))).unwrap(),
            host(&bindings),
            b"{}",
            limits(Duration::from_secs(20)),
        )
        .unwrap_err();
    assert!(!format!("{error:?} {error}").contains("boom"));
}

#[test]
fn a_never_returning_guest_is_interrupted_while_another_store_on_the_engine_completes() {
    let engine = execute::engine().unwrap();
    let executor = Executor::new(&engine).unwrap();
    let spin = Component::new(&engine, spin()).unwrap();
    // Counts params[0..4] (little endian) down to zero, then echoes.
    let busy = Component::new(
        &engine,
        handler(
            "",
            "",
            &format!(
                r#"(local $n i32)
      (local.set $n (i32.load (local.get $ptr)))
      (loop $again
        (local.set $n (i32.sub (local.get $n) (i32.const 1)))
        (br_if $again (local.get $n)))
      {ECHO}"#
            ),
        ),
    )
    .unwrap();
    let bindings = Arc::new(Counting::default());
    let deadline = Duration::from_millis(300);
    let (busy_result, busy_took, spin_result, spin_took) = std::thread::scope(|scope| {
        let busy_run = scope.spawn(|| {
            let started = Instant::now();
            let result = executor.execute(
                &busy,
                host(&bindings),
                &3_000_000_000_u32.to_le_bytes(),
                limits(Duration::from_secs(120)),
            );
            (result, started.elapsed())
        });
        // Start the spinner once the busy guest is already running.
        std::thread::sleep(Duration::from_millis(50));
        let started = Instant::now();
        let spin_result = executor.execute(&spin, host(&bindings), b"{}", limits(deadline));
        let spin_took = started.elapsed();
        let (busy_result, busy_took) = busy_run.join().unwrap();
        (busy_result, busy_took, spin_result, spin_took)
    });
    assert_eq!(spin_result.unwrap_err().code, "timeout");
    assert!(spin_took >= deadline, "{spin_took:?}");
    assert!(
        spin_took < deadline + Duration::from_secs(1),
        "{spin_took:?}"
    );
    // The other store kept running through the spinner's interruption, and finished.
    assert!(
        busy_took > deadline + Duration::from_millis(100),
        "the busy guest must outlive the interruption: {busy_took:?}"
    );
    assert_eq!(busy_result.unwrap(), 3_000_000_000_u32.to_le_bytes());
}

#[test]
fn cancellation_interrupts_a_guest_that_makes_no_host_calls() {
    let engine = execute::engine().unwrap();
    let executor = Executor::new(&engine).unwrap();
    let spin = Component::new(&engine, spin()).unwrap();
    let bindings = Arc::new(Counting::default());
    let host = host(&bindings);
    let cancel = host.cancellation();
    let started = Instant::now();
    let error = std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(100));
            cancel.store(true, Ordering::Release);
        });
        executor
            .execute(&spin, host, b"{}", limits(Duration::from_secs(60)))
            .unwrap_err()
    });
    assert_eq!(error.code, "cancelled");
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// Cancellation is raised inside the guest's last host call; the guest then ends either way.
#[test]
fn cancellation_wins_over_the_handler_outcome_on_success_and_on_error() {
    for (params, what) in [
        (&b"\0"[..], "handler succeeds"),
        (&b"\x01"[..], "handler fails"),
    ] {
        let bindings = Arc::new(Counting::default());
        let host = host(&bindings);
        *bindings.cancel.lock().unwrap() = Some(host.cancellation());
        let result = executor().execute_source(
            &wat::parse_str(log_then_echo_or_fail()).unwrap(),
            host,
            params,
            limits(Duration::from_secs(20)),
        );
        assert_eq!(result.unwrap_err().code, "cancelled", "{what}");
        assert_eq!(bindings.calls.load(Ordering::SeqCst), 1, "{what}");
    }
    // Control: with no cancellation the same guest reports its own outcome.
    let bindings = Arc::new(Counting::default());
    let run = |params: &[u8]| {
        executor().execute_source(
            &wat::parse_str(log_then_echo_or_fail()).unwrap(),
            host(&bindings),
            params,
            limits(Duration::from_secs(20)),
        )
    };
    assert_eq!(run(b"\0").unwrap(), b"\0");
    assert_eq!(run(b"\x01").unwrap_err().code, "handler_failed");
}

/// The deadline expires inside the guest's last host call; the guest then ends either way.
#[test]
fn deadline_expiry_wins_over_the_handler_outcome_on_success_and_on_error() {
    for (params, what) in [
        (&b"\0"[..], "handler succeeds"),
        (&b"\x01"[..], "handler fails"),
    ] {
        let bindings = Arc::new(Counting {
            block: Duration::from_millis(600),
            ..Counting::default()
        });
        let started = Instant::now();
        let result = executor().execute_source(
            &wat::parse_str(log_then_echo_or_fail()).unwrap(),
            host(&bindings),
            params,
            limits(Duration::from_millis(200)),
        );
        assert_eq!(result.unwrap_err().code, "timeout", "{what}");
        assert_eq!(bindings.calls.load(Ordering::SeqCst), 1, "{what}");
        assert!(started.elapsed() < Duration::from_secs(5), "{what}");
    }
}

#[test]
fn an_already_cancelled_invocation_never_runs_the_guest() {
    let bindings = Arc::new(Counting::default());
    let host = host(&bindings);
    host.cancellation().store(true, Ordering::Release);
    let error = executor()
        .execute_source(
            &wat::parse_str(log_then_echo_or_fail()).unwrap(),
            host,
            b"\0",
            limits(Duration::from_secs(20)),
        )
        .unwrap_err();
    assert_eq!(error.code, "cancelled");
    assert_eq!(bindings.calls.load(Ordering::SeqCst), 0);
}

/// A start function that logs, with the handler export removed or mistyped.
fn start_calls_host(handler_export: &str) -> String {
    let source = log_then_echo_or_fail();
    // The core `start` runs when the guest module is instantiated, before any `handle`.
    let with_start = source.replace(
        r#"(data (i32.const 1024) "boomping")"#,
        r#"(data (i32.const 1024) "boomping")
    (func $start (call $log (i32.const 1) (i32.const 1028) (i32.const 4) (i32.const 0) (i32.const 0) (i32.const 64)))
    (start $start)"#,
    );
    assert_ne!(with_start, source);
    match handler_export {
        "none" => {
            let export = r#"(export "falcone:host/handler@0.2.0" (instance $handler))"#;
            assert!(with_start.contains(export));
            with_start.replace(export, "")
        }
        "mistyped" => {
            let params = r#"(param "params" (list u8))"#;
            assert!(with_start.contains(params));
            with_start.replace(params, r#"(param "params" string)"#)
        }
        _ => with_start,
    }
}

#[test]
fn a_start_function_calling_the_host_is_rejected_before_any_instantiation() {
    for variant in ["none", "mistyped"] {
        let bindings = Arc::new(Counting::default());
        let error = executor()
            .execute_source(
                &wat::parse_str(start_calls_host(variant)).unwrap(),
                host(&bindings),
                b"\0",
                limits(Duration::from_secs(20)),
            )
            .unwrap_err();
        assert_eq!(error.code, "missing_handler_export", "{variant}");
        assert_eq!(bindings.calls.load(Ordering::SeqCst), 0, "{variant}");
    }
}

/// Control for the test above: the same start function does reach the host when the
/// component is valid, so the zero count there is not an artifact of the counter.
#[test]
fn the_start_function_does_call_the_host_when_the_handler_is_exact() {
    let bindings = Arc::new(Counting::default());
    let result = executor()
        .execute_source(
            &wat::parse_str(start_calls_host("exact")).unwrap(),
            host(&bindings),
            b"\0",
            limits(Duration::from_secs(20)),
        )
        .unwrap();
    assert_eq!(result, b"\0");
    // One call from the start function, one from handle.
    assert_eq!(bindings.calls.load(Ordering::SeqCst), 2);
}

/// `execute` also refuses a component that skipped the artifact validator, before it
/// instantiates anything.
#[test]
fn execute_checks_the_handler_type_before_instantiating_a_component() {
    let engine = execute::engine().unwrap();
    let executor = Executor::new(&engine).unwrap();
    for variant in ["none", "mistyped"] {
        let bindings = Arc::new(Counting::default());
        let component = Component::new(&engine, start_calls_host(variant)).unwrap();
        let error = executor
            .execute(
                &component,
                host(&bindings),
                b"\0",
                limits(Duration::from_secs(20)),
            )
            .unwrap_err();
        assert_eq!(error.code, "missing_handler_export", "{variant}");
        assert_eq!(bindings.calls.load(Ordering::SeqCst), 0, "{variant}");
    }
}

#[test]
fn wasi_and_old_version_imports_are_still_undeclared() {
    for import in ["wasi:cli/environment@0.2.0", "falcone:host/logging@0.1.0"] {
        let source = echo().replacen(
            "(component\n",
            &format!("(component\n  (import \"{import}\" (instance))\n"),
            1,
        );
        assert_eq!(run(&source, b"{}"), Err("undeclared_import"), "{import}");
    }
}

mod cli {
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("execute-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
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

    fn execute(scratch: &Scratch, component: &Path, params: &Path) -> Output {
        Command::new(env!("CARGO_BIN_EXE_falcone-serverless-worker"))
            .arg("execute")
            .args(["--component".as_ref(), component.as_os_str()])
            .args(["--params".as_ref(), params.as_os_str()])
            .args(["--cache-dir".as_ref(), scratch.0.join("cache").as_os_str()])
            .args(["--deadline-ms", "20000"])
            .output()
            .unwrap()
    }

    #[test]
    fn the_subcommand_writes_the_result_only_on_success() {
        let scratch = Scratch::new("cli");
        let echo = scratch.file(
            "echo.wasm",
            &wat::parse_str(include_str!("fixtures/handler/echo.wat")).unwrap(),
        );
        let params = scratch.file("params.json", br#"{"hello":"world"}"#);
        let output = execute(&scratch, &echo, &params);
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, br#"{"hello":"world"}"#);
        assert!(output.stderr.is_empty());
    }

    #[test]
    fn a_failed_execution_writes_nothing_to_stdout_and_one_bounded_code_to_stderr() {
        let scratch = Scratch::new("cli-fail");
        let oversized = super::handler(
            "",
            "",
            r#"(i32.store (i32.const 0) (i32.const 0))
      (i32.store (i32.const 4) (i32.const 2048))
      (i32.store (i32.const 8) (i32.const 1048577))
      (i32.const 0)"#,
        );
        let component = scratch.file("oversized.wasm", &wat::parse_str(oversized).unwrap());
        let params = scratch.file("params.json", b"{}");
        let output = execute(&scratch, &component, &params);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty(), "{:?}", output.stdout);
        assert_eq!(
            String::from_utf8_lossy(&output.stderr).trim(),
            "limit_exceeded"
        );
        // A rejected component never reaches execution either.
        let rejected = scratch.file("rejected.wasm", b"\0asm\x0d\0\x01\0");
        let output = execute(&scratch, &rejected, &params);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert_eq!(
            String::from_utf8_lossy(&output.stderr).trim(),
            "missing_handler_export"
        );
    }
}

/// The cluster runner embeds these two components as bytes, because the runner binary has no
/// WAT parser. They must stay equal to the sources the unit tests above use.
#[test]
fn cluster_fixtures_match_the_wat_sources() {
    assert_eq!(execute_fixtures::SPIN, wat::parse_str(spin()).unwrap());
    assert_eq!(
        execute_fixtures::START_CALLS_HOST_MISTYPED,
        wat::parse_str(start_calls_host("mistyped")).unwrap()
    );
}
