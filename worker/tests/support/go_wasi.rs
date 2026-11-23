use falcone_serverless_worker::host::{
    self, Bindings, Context, Host, HostError, Operation, Reply, outcome::InvocationOutcome,
};
use falcone_serverless_worker::limits::Limits;
use falcone_serverless_worker::{artifact::supervisor::CompileLimits, cache, server::hooks};
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::{Arc, Mutex, atomic::Ordering};
use std::time::{Duration, Instant};

/// Records the invocation's stdout; WASI reaches no other platform binding.
#[derive(Default)]
pub struct Captured {
    lines: Mutex<Vec<String>>,
    /// Holds the stdout this long first, like a stalled log collector.
    stall: Option<Duration>,
    /// Refuses the stdout, like a full log collector.
    refuse: bool,
    /// Cancels the execution while its stdout is delivered.
    cancel: Option<Arc<AtomicBool>>,
}
impl Bindings for Captured {
    fn call(&self, _: &Context, operation: Operation) -> Result<Reply, HostError> {
        let Operation::Stdout { text } = operation else {
            panic!("WASI must not call other platform bindings")
        };
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Release);
        }
        if let Some(stall) = self.stall {
            std::thread::sleep(stall);
        }
        if self.refuse {
            return Err(HostError::LimitExceeded);
        }
        self.lines.lock().unwrap().push(text);
        Ok(Reply::Empty)
    }
}
fn limits(guest_to_host: u64) -> Limits {
    Limits::parse(&format!(
        r#"{{"version":1,"compiler_cpu_seconds":120,
        "compiler_memory_bytes":4294967296,"guest_memory_bytes":2097152,
        "guest_to_host_bytes":{guest_to_host},"host_to_guest_bytes":1048576,
        "guest_deadline_ms":1000,"host_call_deadline_ms":50,"termination_budget_ms":500}}"#
    ))
    .unwrap()
}
fn host() -> Host {
    Host::new(Context::default(), Arc::new(Captured::default()))
}

pub fn csprng_is_linked_narrowly_and_bounded() {
    let source = r#"(component
      (import "wasi:random/random@0.2.0" (instance $random
        (export "get-random-u64" (func (result u64)))
        (export "get-random-bytes" (func (param "len" u64) (result (list u8))))))
      (core module $mem
        (memory (export "memory") 2)
        (func (export "realloc") (param i32 i32 i32 i32) (result i32) i32.const 1024))
      (core instance $mem (instantiate $mem))
      (alias export $random "get-random-u64" (func $draw))
      (alias export $random "get-random-bytes" (func $bytes))
      (core func $draw (canon lower (func $draw)))
      (core func $bytes (canon lower (func $bytes) (memory $mem "memory") (realloc (func $mem "realloc"))))
      (core module $guest
        (import "host" "draw" (func $draw (result i64)))
        (import "host" "bytes" (func $bytes (param i64 i32)))
        (import "host" "memory" (memory 2))
        (func (export "draw") (result i64) call $draw)
        (func (export "bytes-len") (param i64) (result i32)
          local.get 0 i32.const 0 call $bytes i32.const 4 i32.load))
      (core instance $guest (instantiate $guest (with "host" (instance
        (export "draw" (func $draw)) (export "bytes" (func $bytes))
        (export "memory" (memory $mem "memory"))))))
      (func (export "draw") (result u64) (canon lift (core func $guest "draw")))
      (func (export "bytes-len") (param "len" u64) (result u32) (canon lift (core func $guest "bytes-len"))))"#;
    let engine = wasmtime::Engine::default();
    let (mut store, instance) = host::instantiate(&engine, source, host()).unwrap();
    let draw = instance
        .get_typed_func::<(), (u64,)>(&mut store, "draw")
        .unwrap();
    let a = draw.call(&mut store, ()).unwrap().0;
    let b = draw.call(&mut store, ()).unwrap().0;
    assert_ne!(a, b);
    let bytes = instance
        .get_typed_func::<(u64,), (u32,)>(&mut store, "bytes-len")
        .unwrap();
    assert_eq!(bytes.call(&mut store, (65536,)).unwrap().0, 65536);
    assert!(bytes.call(&mut store, (65537,)).is_err());
    let unlinked = source.replace("get-random-u64", "unlinked");
    assert!(host::instantiate(&engine, unlinked, host()).is_err());
}

pub fn stdout_is_bounded_and_invocation_local() {
    let source = include_str!("../fixtures/go/wasi-stdout.wat");
    let invoke = |length, blocking, guest_to_host: u64, barrier: Option<&std::sync::Barrier>| {
        let engine = wasmtime::Engine::default();
        let captured = Arc::new(Captured::default());
        let host = Host::with_limits(Context::default(), captured.clone(), limits(guest_to_host));
        let (mut store, instance) = host::instantiate(&engine, source, host).unwrap();
        let write = instance
            .get_typed_func::<(u32, u32), (u64,)>(&mut store, "write")
            .unwrap();
        if let Some(barrier) = barrier {
            barrier.wait();
        }
        let remaining = write.call(&mut store, (length, blocking)).unwrap().0;
        if let Some(barrier) = barrier {
            barrier.wait();
        }
        // Nothing leaves while the guest runs; the whole stdout leaves once, at the end.
        assert!(captured.lines.lock().unwrap().is_empty());
        assert!(store.data().flush_stdout());
        assert!(store.data().flush_stdout());
        let lines = captured.lines.lock().unwrap().clone();
        assert_eq!(lines.len(), 1);
        // The fixture writes zero bytes, so each captured byte is one NUL character.
        assert!(lines.iter().all(|line| line.bytes().all(|byte| byte == 0)));
        let logged: usize = lines.iter().map(String::len).sum();
        (remaining, store.data().stdout_summary(), logged)
    };
    // Kept in the invocation's log record up to the bound, then truncated.
    assert_eq!(invoke(4, 0, 1048576, None), (65532, (4, false), 4));
    assert_eq!(invoke(65536, 0, 1048576, None), (0, (65536, false), 65536));
    assert_eq!(invoke(65537, 1, 1048576, None), (0, (65536, true), 65536));
    // A narrowed guest-to-host limit bounds each write before any byte is copied.
    assert_eq!(invoke(1024, 0, 128, None), (128, (128, true), 128));
    assert_eq!(invoke(1024, 1, 128, None), (128, (128, true), 128));
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let a = scope.spawn(|| invoke(123, 0, 1048576, Some(&barrier)));
        let b = scope.spawn(|| invoke(456, 1, 1048576, Some(&barrier)));
        assert_eq!(a.join().unwrap(), (65413, (123, false), 123));
        assert_eq!(b.join().unwrap(), (65080, (456, false), 456));
    });
}

pub fn stdout_flush_is_supervised_and_reports_refusal() {
    let source = include_str!("../fixtures/go/wasi-stdout.wat");
    let run = |captured: Captured, cancel: bool| {
        let engine = wasmtime::Engine::default();
        let captured = Arc::new(captured);
        let host = Host::with_limits(Context::default(), captured.clone(), limits(1048576));
        let (mut store, instance) = host::instantiate(&engine, source, host).unwrap();
        let write = instance
            .get_typed_func::<(u32, u32), (u64,)>(&mut store, "write")
            .unwrap();
        write.call(&mut store, (4, 0)).unwrap();
        if cancel {
            store.data().cancellation().store(true, Ordering::Release);
        }
        let started = Instant::now();
        let delivered = store.data().flush_stdout();
        let elapsed = started.elapsed();
        // Nothing is retried after a failed delivery.
        assert!(store.data().flush_stdout());
        // Stdout has no external effect, so an unacknowledged line never makes a pure
        // handler's outcome uncertain.
        let outcome = host::outcome::classify(store.data(), Ok(()));
        assert_ne!(outcome, InvocationOutcome::Uncertain);
        (
            delivered,
            elapsed,
            captured.lines.lock().unwrap().len(),
            outcome,
        )
    };
    // A stalled collector is abandoned at the 50 ms host-call deadline.
    let stalled = Captured {
        stall: Some(Duration::from_secs(5)),
        ..Captured::default()
    };
    let (delivered, elapsed, lines, outcome) = run(stalled, false);
    assert!(!delivered && lines == 0, "{delivered} {lines}");
    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
    assert_eq!(outcome, InvocationOutcome::Cancelled);
    // Cancellation stops the flush before the collector is reached.
    let (delivered, _, _, outcome) = run(Captured::default(), true);
    assert!(!delivered);
    assert_eq!(outcome, InvocationOutcome::Cancelled);
    // A collector that drops the line is reported, so truncation is flagged; the
    // handler's own outcome is unaffected.
    let refusing = Captured {
        refuse: true,
        ..Captured::default()
    };
    let (delivered, _, _, outcome) = run(refusing, false);
    assert!(!delivered);
    assert_eq!(outcome, InvocationOutcome::Success);
}

/// The stdout fixture with `data` at the address its plain writes start from; blocking
/// writes start 4,096 bytes later.
fn stdout_with(data: &str) -> String {
    include_str!("../fixtures/go/wasi-stdout.wat")
        .replace(
            r#"(core module $mem (memory (export "memory") 2))"#,
            &format!(r#"(core module $mem (memory (export "memory") 2) (data (i32.const 1024) "{data}"))"#),
        )
        .replace(
            "local.get $handle i32.const 1024 local.get $len i32.const 0 call $blocking",
            "local.get $handle i32.const 5120 local.get $len i32.const 0 call $blocking",
        )
}

/// Runs `writes` of (length, blocking) against the worker's own log record and returns the
/// record's stdout text with each line's framing removed, and the stdout metadata.
fn recorded(data: &str, secrets: &[&str], writes: &[(u32, u32)]) -> (String, (usize, bool)) {
    let engine = wasmtime::Engine::default();
    let (bindings, lines) = hooks::log_record(secrets);
    let host = Host::with_limits(Context::default(), bindings, limits(1048576));
    let (mut store, instance) = host::instantiate(&engine, stdout_with(data), host).unwrap();
    let write = instance
        .get_typed_func::<(u32, u32), (u64,)>(&mut store, "write")
        .unwrap();
    for write_args in writes {
        write.call(&mut store, *write_args).unwrap();
    }
    assert!(store.data().flush_stdout());
    let summary = store.data().stdout_summary();
    let text = lines()
        .iter()
        .map(|line| {
            line.strip_prefix("info ")
                .and_then(|line| line.strip_suffix(" stream=stdout"))
                .unwrap()
                .to_owned()
        })
        .collect();
    (text, summary)
}

pub fn stdout_reaches_the_record_redacted_and_whole() {
    // A short claim straddles the first 4,096-character line cut and a long one the second.
    // The record redacts the whole stdout before it cuts, so neither survives in any line
    // or across adjacent lines.
    let (short, long) = ("pw1234", "abcdefghijkl");
    let padding = |n| "a".repeat(n);
    let data = format!("{}{short}{}{long}", padding(4093), padding(4089));
    let (text, summary) = recorded(&data, &[short, long], &[(8200, 0)]);
    assert_eq!(
        text,
        format!("{}<redacted>{}<redacted>", padding(4093), padding(4089))
    );
    assert_eq!(summary, (8200, false));
    // A character split between two writes at the line size is decoded whole.
    let data = format!(r"{}\c3\a9", padding(4095));
    let (text, summary) = recorded(&data, &[], &[(4096, 0), (1, 1)]);
    assert_eq!(text, format!("{}é", padding(4095)));
    assert_eq!(summary, (4097, false));
}

/// Fixture compilation through the bounded trusted compile path, in a scratch cache.
struct Compiler {
    cache: cache::Cache,
    directory: std::path::PathBuf,
}
impl Compiler {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let directory =
            std::env::temp_dir().join(format!("go-wasi-cache-{}-{id}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let exe = std::env::current_exe().unwrap();
        let parent = exe.parent().unwrap();
        // Cargo integration tests live in deps/; in the restricted pod the test binary is
        // beside the compiler.
        let binaries = if parent.file_name().is_some_and(|name| name == "deps") {
            parent.parent().unwrap()
        } else {
            parent
        };
        let cache = cache::Cache::new(&directory, binaries.join("artifact-compile-child")).unwrap();
        Self { cache, directory }
    }
    fn compile(&mut self, bytes: &[u8]) -> cache::Loaded {
        let limits = CompileLimits {
            input_bytes: 64 * 1024 * 1024,
            cpu_seconds: 120,
            memory_bytes: 4 * 1024 * 1024 * 1024,
            wall_clock_deadline: Instant::now() + Duration::from_secs(300),
        };
        self.cache
            .compile(
                cache::CONTRACT_VERSION,
                bytes,
                limits,
                &AtomicBool::new(false),
            )
            .unwrap()
    }
}
impl Drop for Compiler {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// A handler that writes `length` stdout bytes and returns, or traps after the write.
fn writing_handler(length: u32, trap: bool) -> Vec<u8> {
    let source = include_str!("../fixtures/go/wasi-stdout.wat");
    let (prefix, _) = source.rsplit_once("  (func (export \"write\")").unwrap();
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
    wat::parse_str(source).unwrap()
}

#[derive(Clone, Copy, Debug)]
enum Collector {
    Keeps,
    CancelsWhileKeeping,
    Stalls,
    Refuses,
    StallsPastTheDeadline,
}

pub fn execution_end_delivers_and_classifies_stdout() {
    // Every execution ends in the same finish step: stdout leaves even on a trap, and a
    // cancellation, call timeout, refusal or deadline met while it leaves is classified
    // after it, never as uncertain. The returned metadata reports what was lost.
    use Collector::*;
    use InvocationOutcome::*;
    let mut compiler = Compiler::new();
    for (length, trap, collector, outcome, metadata) in [
        (4, false, Keeps, Success, (4, false)),
        (65537, false, Keeps, Success, (65536, true)),
        (65537, true, Keeps, Failed, (65536, true)),
        (4, false, CancelsWhileKeeping, Cancelled, (4, false)),
        (4, false, Stalls, Cancelled, (4, true)),
        (4, false, Refuses, Success, (4, true)),
        // The execution deadline takes precedence over the abandoned call.
        (4, false, StallsPastTheDeadline, GuestInterrupted, (4, true)),
    ] {
        let loaded = compiler.compile(&writing_handler(length, trap));
        let flag = Arc::new(AtomicBool::new(false));
        let captured = Arc::new(Captured {
            stall: matches!(collector, Stalls | StallsPastTheDeadline)
                .then(|| Duration::from_secs(1)),
            refuse: matches!(collector, Refuses),
            cancel: matches!(collector, CancelsWhileKeeping).then(|| flag.clone()),
            ..Captured::default()
        });
        let mut host = Host::new(Context::default(), captured.clone());
        host.set_cancellation(flag);
        if matches!(collector, StallsPastTheDeadline) {
            host.set_call_timeout(Duration::from_secs(5));
            host.set_until(Instant::now() + Duration::from_millis(300));
        } else {
            host.set_call_timeout(Duration::from_millis(50));
        }
        let execution = host::execute(&loaded, host, b"");
        let case = format!("{length} {trap} {collector:?}: {execution:?}");
        assert_eq!(execution.outcome, outcome, "{case}");
        assert_eq!(execution.result.is_err(), outcome != Success, "{case}");
        assert_eq!(
            (execution.stdout_bytes, execution.stdout_truncated),
            metadata,
            "{case}"
        );
        let delivered = captured.lines.lock().unwrap().clone();
        let kept = matches!(collector, Keeps | CancelsWhileKeeping);
        assert_eq!(delivered.len(), usize::from(kept), "{case}");
    }
}

pub fn one_memory_and_only_declared_functions_are_linked() {
    let engine = wasmtime::Engine::default();
    let two_memories = r#"(component
      (core module $mem (memory 1))
      (core instance (instantiate $mem))
      (core instance (instantiate $mem)))"#;
    assert!(host::instantiate(&engine, two_memories, host()).is_err());
    let unlinked = include_str!("../fixtures/go/wasi-stdout.wat").replace(
        "[method]output-stream.write",
        "[method]output-stream.unlinked",
    );
    assert!(host::instantiate(&engine, unlinked, host()).is_err());
}
