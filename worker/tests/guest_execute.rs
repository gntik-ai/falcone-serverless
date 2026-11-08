//! Guest execution through the exported `falcone:host/handler@0.2.0` interface.
//!
//! Fixtures are our own WAT components, compiled here with the trusted engine
//! configuration; tenant bytes only ever reach the worker through the CLI tests below,
//! which go through the validator and the supervised compiler.
#![cfg(target_os = "linux")]

use falcone_serverless_worker::host::execute::{
    Executor, Failure, Limits, MAX_ERROR_MESSAGE_BYTES, Unbound, engine,
};
use falcone_serverless_worker::host::{
    BUFFER_BYTES, Bindings, Context, Host, HostError, Operation, Reply,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use wasmtime::component::Component;

const BUMP_REALLOC: &str = r#"
      (local $ptr i32)
      (if (i32.gt_u (local.get 3) (i32.sub (i32.const 1179648) (global.get $heap)))
        (then unreachable))
      (local.set $ptr (global.get $heap))
      (global.set $heap (i32.add (local.get $ptr) (local.get 3)))
      (local.get $ptr)"#;

// result<list<u8>, handler-error> is returned through memory at 16: the tag at 16, then the
// list (ptr at 20, len at 24) or the error (case at 20, string ptr at 24, len at 28).
const ECHO: &str = r#"
      (i32.store8 (i32.const 16) (i32.const 0))
      (i32.store (i32.const 20) (local.get $ptr))
      (i32.store (i32.const 24) (local.get $len))
      (i32.const 16)"#;
const PLUS_ONE: &str = r#"
      (i32.store8 (i32.const 16) (i32.const 0))
      (i32.store (i32.const 20) (local.get $ptr))
      (i32.store (i32.const 24) (i32.add (local.get $len) (i32.const 1)))
      (i32.const 16)"#;
const TRAP: &str = "unreachable";
const SPIN: &str = "(loop $l (br $l)) (i32.const 0)";
const INVALID_INPUT: &str = r#"
      (i32.store8 (i32.const 16) (i32.const 1))
      (i32.store8 (i32.const 20) (i32.const 0))
      (i32.store (i32.const 24) (i32.const 32))
      (i32.store (i32.const 28) (i32.const 10))
      (i32.const 16)"#;
const FAILED_LONG: &str = r#"
      (memory.fill (i32.const 1024) (i32.const 97) (i32.const 70000))
      (i32.store8 (i32.const 16) (i32.const 1))
      (i32.store8 (i32.const 20) (i32.const 1))
      (i32.store (i32.const 24) (i32.const 1024))
      (i32.store (i32.const 28) (i32.const 70000))
      (i32.const 16)"#;
const BAD_LIST: &str = r#"
      (i32.store8 (i32.const 16) (i32.const 0))
      (i32.store (i32.const 20) (i32.const 0))
      (i32.store (i32.const 24) (i32.const 0x7fffffff))
      (i32.const 16)"#;

fn handler_component(imports: &str, realloc: &str, handle: &str) -> String {
    format!(
        r#"(component
  (type $host (instance))
  {imports}
  (core module $m
    (memory (export "memory") 18)
    (global $heap (mut i32) (i32.const 64))
    (data (i32.const 32) "bad params")
    (func (export "realloc") (param i32 i32 i32 i32) (result i32) {realloc})
    (func (export "handle") (param $ptr i32) (param $len i32) (result i32) {handle}))
  (core instance $i (instantiate $m))
  (type $handler-error (variant (case "invalid-input" string) (case "failed" string)))
  (type $handle-type (func (param "params" (list u8)) (result (result (list u8) (error $handler-error)))))
  (func $handle (type $handle-type)
    (canon lift (core func $i "handle") (memory $i "memory") (realloc (func $i "realloc"))))
  (instance $handler
    (export "handler-error" (type $handler-error))
    (export "handle" (func $handle)))
  (export "falcone:host/handler@0.2.0" (instance $handler)))"#
    )
}

fn executor() -> Executor {
    Executor::new(engine().unwrap()).unwrap()
}

fn load(executor: &Executor, handle: &str) -> Component {
    let wat = handler_component("", BUMP_REALLOC, handle);
    Component::new(executor.engine(), wat).unwrap()
}

fn host() -> Host {
    Host::new(Context::default(), Arc::new(Unbound))
}

fn limits() -> Limits {
    Limits {
        deadline: Duration::from_secs(30),
        fuel: u64::MAX,
    }
}

fn run(executor: &Executor, component: &Component, params: &[u8]) -> Result<Vec<u8>, Failure> {
    executor.execute(component, host(), params, limits())
}

fn code(result: Result<Vec<u8>, Failure>) -> &'static str {
    result.expect_err("execution must fail").code
}

#[test]
fn echo_returns_the_params_byte_for_byte() {
    let executor = executor();
    let component = load(&executor, ECHO);
    let params = br#"{"op":"echo","n":[1,2,3],"s":"x"}"#;
    assert_eq!(run(&executor, &component, params).unwrap(), params);
    assert_eq!(run(&executor, &component, b"").unwrap(), b"");
    // The same compiled component serves repeated invocations from fresh stores.
    assert_eq!(run(&executor, &component, b"again").unwrap(), b"again");
}

#[test]
fn params_are_limited_at_exactly_max_host_to_guest_bytes() {
    assert_eq!(BUFFER_BYTES, 1_048_576);
    let executor = executor();
    let component = load(&executor, ECHO);
    let exact = vec![7_u8; 1_048_576];
    assert_eq!(run(&executor, &component, &exact).unwrap(), exact);
    let over = vec![7_u8; 1_048_577];
    assert_eq!(code(run(&executor, &component, &over)), "limit_exceeded");
}

#[test]
fn oversized_params_are_rejected_before_guest_memory_is_written() {
    // Writing params into the guest calls its realloc, which here always traps. Seeing
    // limit_exceeded instead of trap proves the guest was never entered.
    let executor = executor();
    let wat = handler_component("", "unreachable", ECHO);
    let component = Component::new(executor.engine(), wat).unwrap();
    let over = vec![0_u8; 1_048_577];
    assert_eq!(code(run(&executor, &component, &over)), "limit_exceeded");
    // Control: within the limit the same component does reach realloc and traps.
    assert_eq!(code(run(&executor, &component, b"x")), "trap");
}

#[test]
fn results_are_limited_at_exactly_max_guest_to_host_bytes() {
    let executor = executor();
    let component = load(&executor, PLUS_ONE);
    // params + 1 byte comes back: 1048575 + 1 is exactly the ceiling, 1048576 + 1 is over.
    let at_limit = run(&executor, &component, &vec![9_u8; 1_048_575]).unwrap();
    assert_eq!(at_limit.len(), 1_048_576);
    assert_eq!(&at_limit[..1_048_575], &vec![9_u8; 1_048_575][..]);
    let result = run(&executor, &component, &vec![9_u8; 1_048_576]);
    assert_eq!(code(result), "limit_exceeded");
}

#[test]
fn a_result_outside_guest_memory_is_a_trap_not_partial_output() {
    let executor = executor();
    let component = load(&executor, BAD_LIST);
    assert_eq!(code(run(&executor, &component, b"x")), "trap");
}

#[test]
fn trap_maps_to_one_stable_code_without_output() {
    let executor = executor();
    let component = load(&executor, TRAP);
    let failure = run(&executor, &component, b"x").unwrap_err();
    assert_eq!(
        failure,
        Failure {
            code: "trap",
            message: None
        }
    );
}

#[test]
fn handler_errors_map_to_stable_codes_and_bounded_messages() {
    let executor = executor();
    let failure = run(&executor, &load(&executor, INVALID_INPUT), b"x").unwrap_err();
    assert_eq!(failure.code, "handler_invalid_input");
    assert_eq!(failure.message.as_deref(), Some("bad params"));
    let failure = run(&executor, &load(&executor, FAILED_LONG), b"x").unwrap_err();
    assert_eq!(failure.code, "handler_failed");
    assert_eq!(MAX_ERROR_MESSAGE_BYTES, 256);
    assert_eq!(failure.message.unwrap(), "a".repeat(256));
}

#[test]
fn out_of_fuel_is_a_limit() {
    let executor = executor();
    let component = load(&executor, SPIN);
    let limits = Limits {
        deadline: Duration::from_secs(30),
        fuel: 10_000,
    };
    let result = executor.execute(&component, host(), b"x", limits);
    assert_eq!(code(result), "limit_exceeded");
}

#[test]
fn never_returning_handle_is_interrupted_at_the_deadline() {
    let executor = executor();
    let component = load(&executor, SPIN);
    let deadline = Duration::from_millis(300);
    let started = Instant::now();
    let result = executor.execute(
        &component,
        host(),
        b"x",
        Limits {
            deadline,
            fuel: u64::MAX,
        },
    );
    let elapsed = started.elapsed();
    assert_eq!(code(result), "timeout");
    assert!(
        elapsed >= deadline - Duration::from_millis(20),
        "{elapsed:?}"
    );
    assert!(elapsed < deadline + Duration::from_secs(2), "{elapsed:?}");
}

#[test]
fn a_timed_out_store_does_not_stop_another_store_on_the_same_engine() {
    let executor = Arc::new(executor());
    let spin = Arc::new(load(&executor, SPIN));
    let echo = Arc::new(load(&executor, ECHO));
    let stop = Arc::new(AtomicBool::new(false));
    let other = {
        let (executor, echo, stop) = (executor.clone(), echo.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut completed = 0_u32;
            while !stop.load(Ordering::Acquire) {
                let params = completed.to_le_bytes();
                // A long deadline: only an engine-wide interruption could stop this store.
                let result = executor.execute(&echo, host(), &params, limits());
                assert_eq!(result.unwrap(), params);
                completed += 1;
            }
            completed
        })
    };
    let started = Instant::now();
    let result = executor.execute(
        &spin,
        host(),
        b"x",
        Limits {
            deadline: Duration::from_millis(300),
            fuel: u64::MAX,
        },
    );
    assert_eq!(code(result), "timeout");
    assert!(started.elapsed() < Duration::from_secs(3));
    stop.store(true, Ordering::Release);
    let completed = other.join().expect("echo store must not fail");
    assert!(completed > 0, "the other store made no progress");
}

#[test]
fn cancellation_interrupts_a_running_guest_and_blocks_a_new_one() {
    let executor = executor();
    let component = load(&executor, SPIN);
    let running = host();
    let flag = running.cancellation();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        flag.store(true, Ordering::Release);
    });
    let started = Instant::now();
    let result = executor.execute(&component, running, b"x", limits());
    canceller.join().unwrap();
    assert_eq!(code(result), "cancelled");
    assert!(started.elapsed() < Duration::from_secs(5));
    // Already cancelled: the guest is never entered, so even a trapping realloc is unseen.
    let wat = handler_component("", "unreachable", ECHO);
    let component = Component::new(executor.engine(), wat).unwrap();
    let cancelled = host();
    cancelled.cancellation().store(true, Ordering::Release);
    let result = executor.execute(&component, cancelled, b"x", limits());
    assert_eq!(code(result), "cancelled");
}

#[test]
fn a_cancelled_invocation_returns_no_output_even_if_the_guest_finishes() {
    let executor = executor();
    let component = load(&executor, ECHO);
    let finished = host();
    finished.cancellation().store(true, Ordering::Release);
    assert_eq!(
        code(executor.execute(&component, finished, b"x", limits())),
        "cancelled"
    );
}

#[test]
fn the_linker_stays_deny_by_default_for_foreign_imports() {
    let executor = executor();
    let imports = r#"(import "wasi:filesystem/types@0.2.0" (instance (type $host)))"#;
    let wat = handler_component(imports, BUMP_REALLOC, ECHO);
    let component = Component::new(executor.engine(), wat).unwrap();
    assert_eq!(code(run(&executor, &component, b"x")), "instantiate_failed");
    // A falcone interface the grants do not cover is denied as well.
    let imports = r#"(import "falcone:host/storage@0.2.0" (instance (type $host)))"#;
    let wat = handler_component(imports, BUMP_REALLOC, ECHO);
    let component = Component::new(executor.engine(), wat).unwrap();
    assert_eq!(code(run(&executor, &component, b"x")), "instantiate_failed");
}

#[test]
fn a_component_without_the_handler_export_is_not_executed() {
    let executor = executor();
    let component = Component::new(executor.engine(), "(component)").unwrap();
    assert_eq!(
        code(run(&executor, &component, b"x")),
        "missing_handler_export"
    );
}

/// A binding that answers the first `progress.report` after `first` and every later one
/// after `later`, the way a slow platform service would.
struct Slow {
    calls: AtomicUsize,
    first: Duration,
    later: Duration,
}

impl Bindings for Slow {
    fn call(&self, _context: &Context, _operation: Operation) -> Result<Reply, HostError> {
        let delay = if self.calls.fetch_add(1, Ordering::AcqRel) == 0 {
            self.first
        } else {
            self.later
        };
        std::thread::sleep(delay);
        Ok(Reply::Empty)
    }
}

/// A handler that calls the ungated `progress.report` twice, then echoes its params.
fn reporting_component() -> String {
    format!(
        r#"(component
  (type $progress (instance
    (type $host-error (variant (case "denied") (case "limit-exceeded") (case "timeout") (case "cancelled")))
    (export "host-error" (type $herr (eq $host-error)))
    (export "report" (func (param "completed" u64) (param "total" u64) (param "message" string) (result (result (error $herr)))))))
  (import "falcone:host/progress@0.2.0" (instance $p (type $progress)))
  (core module $mem (memory (export "memory") 18))
  (core instance $m (instantiate $mem))
  (alias export $p "report" (func $report))
  (core func $report-lowered (canon lower (func $report) (memory $m "memory")))
  (core module $main
    (import "m" "memory" (memory 18))
    (import "p" "report" (func $report (param i64 i64 i32 i32 i32)))
    (global $heap (mut i32) (i32.const 64))
    (func (export "realloc") (param i32 i32 i32 i32) (result i32) {BUMP_REALLOC})
    (func (export "handle") (param $ptr i32) (param $len i32) (result i32)
      (call $report (i64.const 0) (i64.const 0) (i32.const 32) (i32.const 0) (i32.const 256))
      (call $report (i64.const 1) (i64.const 1) (i32.const 32) (i32.const 0) (i32.const 256))
      {ECHO}))
  (core instance $i (instantiate $main
    (with "m" (instance $m))
    (with "p" (instance (export "report" (func $report-lowered))))))
  (type $handler-error (variant (case "invalid-input" string) (case "failed" string)))
  (type $handle-type (func (param "params" (list u8)) (result (result (list u8) (error $handler-error)))))
  (func $handle (type $handle-type)
    (canon lift (core func $i "handle") (memory $m "memory") (realloc (func $i "realloc"))))
  (instance $handler
    (export "handler-error" (type $handler-error))
    (export "handle" (func $handle)))
  (export "falcone:host/handler@0.2.0" (instance $handler)))"#
    )
}

#[test]
fn a_late_blocking_host_call_cannot_outlive_the_invocation_deadline() {
    let executor = executor();
    let component = Component::new(executor.engine(), reporting_component()).unwrap();
    // Sanity: with fast bindings the same guest completes and echoes.
    let fast = Host::new(
        Context::default(),
        Arc::new(Slow {
            calls: AtomicUsize::new(0),
            first: Duration::ZERO,
            later: Duration::ZERO,
        }),
    );
    let output = executor.execute(&component, fast, b"ok", limits()).unwrap();
    assert_eq!(output, b"ok");
    // The first call spends most of the 600 ms allowance; the second would block for seconds.
    // Restarting the per-call timeout would let it run to about 1050 ms.
    let deadline = Duration::from_millis(600);
    let slow = Host::new(
        Context::default(),
        Arc::new(Slow {
            calls: AtomicUsize::new(0),
            first: Duration::from_millis(450),
            later: Duration::from_secs(5),
        }),
    );
    let started = Instant::now();
    let result = executor.execute(
        &component,
        slow,
        b"x",
        Limits {
            deadline,
            fuel: u64::MAX,
        },
    );
    let elapsed = started.elapsed();
    assert_eq!(code(result), "timeout");
    assert!(
        elapsed < deadline + Duration::from_millis(250),
        "{elapsed:?}"
    );
}

mod cli {
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output, Stdio};

    struct Dir(PathBuf);
    impl Dir {
        fn new(name: &str) -> Self {
            let path = Path::new(env!("CARGO_TARGET_TMPDIR"))
                .join(format!("execute-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn component(handle: &str) -> Vec<u8> {
        wat::parse_str(super::handler_component("", super::BUMP_REALLOC, handle)).unwrap()
    }

    fn execute(dir: &Dir, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_falcone-serverless-worker"));
        command
            .arg("execute")
            .args(args)
            .env_clear()
            .env("TMPDIR", &dir.0)
            .stdin(Stdio::null());
        command.output().unwrap()
    }

    fn stderr(output: &Output) -> String {
        String::from_utf8_lossy(&output.stderr).into_owned()
    }

    #[test]
    fn echo_fixture_returns_params_and_exits_zero() {
        let dir = Dir::new("echo");
        let artifact = dir.write("echo.wasm", &component(super::ECHO));
        let params = dir.write("params.json", br#"{"hello":"world"}"#);
        let output = execute(
            &dir,
            &[artifact.to_str().unwrap(), params.to_str().unwrap()],
        );
        assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
        assert_eq!(output.stdout, br#"{"hello":"world"}"#);
        assert_eq!(stderr(&output), "");
    }

    #[test]
    fn oversized_params_fail_with_the_limit_code_and_no_stdout() {
        let dir = Dir::new("limit");
        let artifact = dir.write("echo.wasm", &component(super::ECHO));
        let params = dir.write("params.bin", &vec![1_u8; 1_048_577]);
        let output = execute(
            &dir,
            &[artifact.to_str().unwrap(), params.to_str().unwrap()],
        );
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(output.stdout, b"");
        assert_eq!(stderr(&output), "execute failed: limit_exceeded\n");
    }

    #[test]
    fn oversized_result_fails_with_the_limit_code_and_no_stdout() {
        let dir = Dir::new("result-limit");
        let artifact = dir.write("plus.wasm", &component(super::PLUS_ONE));
        let params = dir.write("params.bin", &vec![1_u8; 1_048_576]);
        let output = execute(
            &dir,
            &[artifact.to_str().unwrap(), params.to_str().unwrap()],
        );
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(output.stdout, b"");
        assert_eq!(stderr(&output), "execute failed: limit_exceeded\n");
    }

    #[test]
    fn a_trap_exits_nonzero_with_the_stable_code_and_no_stdout() {
        let dir = Dir::new("trap");
        let artifact = dir.write("trap.wasm", &component(super::TRAP));
        let params = dir.write("params.json", b"{}");
        let output = execute(
            &dir,
            &[artifact.to_str().unwrap(), params.to_str().unwrap()],
        );
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(output.stdout, b"");
        assert_eq!(stderr(&output), "execute failed: trap\n");
    }

    #[test]
    fn a_handler_error_prints_its_bounded_message_escaped() {
        let dir = Dir::new("handler-error");
        let artifact = dir.write("e.wasm", &component(super::INVALID_INPUT));
        let params = dir.write("params.json", b"{}");
        let output = execute(
            &dir,
            &[artifact.to_str().unwrap(), params.to_str().unwrap()],
        );
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(output.stdout, b"");
        assert_eq!(
            stderr(&output),
            "execute failed: handler_invalid_input: \"bad params\"\n"
        );
    }

    #[test]
    fn the_deadline_option_interrupts_a_never_returning_guest() {
        let dir = Dir::new("deadline");
        let artifact = dir.write("spin.wasm", &component(super::SPIN));
        let params = dir.write("params.json", b"{}");
        let started = std::time::Instant::now();
        let output = execute(
            &dir,
            &[
                "--deadline-ms",
                "200",
                artifact.to_str().unwrap(),
                params.to_str().unwrap(),
            ],
        );
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(output.stdout, b"");
        assert_eq!(stderr(&output), "execute failed: timeout\n");
        assert!(started.elapsed() < std::time::Duration::from_secs(30));
    }

    #[test]
    fn native_and_non_conforming_artifacts_are_never_loaded() {
        let dir = Dir::new("reject");
        let params = dir.write("params.json", b"{}");
        let engine = wasmtime::Engine::new(
            wasmtime::Config::new()
                .consume_fuel(true)
                .epoch_interruption(true),
        )
        .unwrap();
        let precompiled = engine
            .precompile_component(&component(super::ECHO))
            .unwrap();
        let mut elf = b"\x7fELF\x02\x01\x01\0".to_vec();
        elf.resize(64, 0);
        let wasi = wat::parse_str(super::handler_component(
            r#"(import "wasi:filesystem/types@0.2.0" (instance (type $host)))"#,
            super::BUMP_REALLOC,
            super::ECHO,
        ))
        .unwrap();
        let no_handler = wat::parse_str("(component)").unwrap();
        for (name, bytes, expected) in [
            ("precompiled.cwasm", precompiled, "precompiled_rejected"),
            ("native.elf", elf, "precompiled_rejected"),
            ("wasi.wasm", wasi, "undeclared_import"),
            ("empty.wasm", no_handler, "missing_handler_export"),
            (
                "garbage.wasm",
                b"\0asm\x0d\0\x01\0\xff".to_vec(),
                "invalid_module",
            ),
        ] {
            let artifact = dir.write(name, &bytes);
            let output = execute(
                &dir,
                &[artifact.to_str().unwrap(), params.to_str().unwrap()],
            );
            assert_eq!(output.status.code(), Some(1), "{name}");
            assert_eq!(output.stdout, b"", "{name}");
            let message = stderr(&output);
            assert!(
                message.starts_with(&format!("execute failed: {expected}")),
                "{name}: {message}"
            );
            // Diagnostics are bounded and never carry artifact bytes.
            assert!(message.len() < 512, "{name}: {message}");
        }
    }

    #[test]
    fn usage_errors_exit_nonzero_with_usage_text() {
        let dir = Dir::new("usage");
        for args in [
            &[][..],
            &["only-one"],
            &["--deadline-ms", "0", "a", "b"],
            &["--deadline-ms", "x", "a", "b"],
        ] {
            let output = execute(&dir, args);
            assert_eq!(output.status.code(), Some(1), "{args:?}");
            assert_eq!(output.stdout, b"", "{args:?}");
            assert!(
                stderr(&output).starts_with("usage: falcone-serverless-worker"),
                "{args:?}"
            );
        }
        let output = execute(
            &dir,
            &["/nonexistent/component.wasm", "/nonexistent/params"],
        );
        assert_eq!(output.status.code(), Some(1));
        assert!(stderr(&output).starts_with("execute failed: input_unreadable"));
    }
}
