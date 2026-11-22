use falcone_serverless_worker::host::{
    self, Bindings, Context, Host, HostError, Operation, Reply, outcome::InvocationOutcome,
};
use falcone_serverless_worker::limits::Limits;
use std::sync::{Arc, Mutex, atomic::Ordering};
use std::time::{Duration, Instant};

/// Records the invocation's stdout lines; WASI reaches no other platform binding.
#[derive(Default)]
pub struct Captured {
    lines: Mutex<Vec<String>>,
    /// Holds each line this long first, like a stalled log collector.
    stall: Option<Duration>,
    /// Refuses each line, like a full log collector.
    refuse: bool,
}
impl Bindings for Captured {
    fn call(&self, _: &Context, operation: Operation) -> Result<Reply, HostError> {
        let Operation::Stdout { text } = operation else {
            panic!("WASI must not call other platform bindings")
        };
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
        // Complete lines leave while the guest runs; the rest when the invocation ends.
        let (accepted, _) = store.data().stdout_summary();
        let early = captured.lines.lock().unwrap().len();
        assert_eq!(early, accepted / host::STDOUT_LINE_CHARS);
        assert!(store.data().flush_stdout(true));
        assert!(store.data().flush_stdout(true));
        let lines = captured.lines.lock().unwrap().clone();
        // The fixture writes zero bytes, so each captured byte is one NUL character.
        assert!(lines.iter().all(|line| line.bytes().all(|byte| byte == 0)));
        assert!(
            lines
                .iter()
                .all(|line| line.len() <= host::STDOUT_LINE_CHARS)
        );
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
        let delivered = store.data().flush_stdout(true);
        let elapsed = started.elapsed();
        // Nothing is retried after a failed line.
        assert!(store.data().flush_stdout(true));
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
