use crate::host::{self, Bindings, Context, Grants, Host, HostError, Operation, Reply};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;
use wasmtime as engine;

const TOLERANCE: Duration = Duration::from_secs(2);

struct DelayedMutation {
    dispatched: mpsc::SyncSender<()>,
    release: Mutex<mpsc::Receiver<()>>,
    committed: mpsc::SyncSender<()>,
}
impl Bindings for DelayedMutation {
    fn call(&self, _: &Context, operation: Operation) -> Result<Reply, HostError> {
        assert!(matches!(operation, Operation::Publish { .. }));
        self.dispatched.send(()).unwrap();
        self.release.lock().unwrap().recv().unwrap();
        self.committed.send(()).unwrap();
        Ok(Reply::Empty)
    }
}

#[cfg_attr(test, test)]
pub fn cancellation_after_mutation_dispatch_never_claims_plain_cancelled() {
    let (dispatched, started) = mpsc::sync_channel(1);
    let (release, released) = mpsc::sync_channel(1);
    let (committed, observed) = mpsc::sync_channel(1);
    let binding = Arc::new(DelayedMutation {
        dispatched,
        release: Mutex::new(released),
        committed,
    });
    let context = Context {
        grants: Grants {
            topics: vec!["orders".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let host = Host::new(context, binding);
    let cancel: Arc<AtomicBool> = host.cancellation();
    let engine = engine::Engine::default();
    let (mut store, instance) = host::instantiate(
        &engine,
        include_str!("limits_fixtures/delayed-mutation.wat"),
        host,
    )
    .unwrap();
    let run = instance
        .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
        .unwrap();
    let result = std::thread::scope(|scope| {
        scope.spawn(move || {
            started.recv_timeout(Duration::from_secs(5)).unwrap();
            cancel.store(true, Ordering::Release);
        });
        run.call(&mut store, (0,)).unwrap()
    });
    release.send(()).unwrap();
    observed.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(result, (4,)); // The WIT interface is unchanged.
    assert_eq!(
        host::outcome::classify(store.data(), Ok(())),
        host::outcome::InvocationOutcome::Uncertain
    );
}

fn small_limits() -> crate::limits::Limits {
    crate::limits::Limits::parse(
        r#"{"version":1,"compiler_cpu_seconds":120,
        "compiler_memory_bytes":4294967296,"guest_memory_bytes":2097152,
        "guest_to_host_bytes":128,"host_to_guest_bytes":128,"guest_deadline_ms":100,
        "host_call_deadline_ms":50,"termination_budget_ms":500}"#,
    )
    .unwrap()
}

struct Storage {
    calls: std::sync::atomic::AtomicUsize,
    reply_bytes: usize,
}
impl Bindings for Storage {
    fn call(&self, _: &Context, op: Operation) -> Result<Reply, HostError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match op {
            Operation::Read { .. } => Ok(Reply::Data(vec![42; self.reply_bytes])),
            Operation::Write { .. } => Ok(Reply::Empty),
            _ => Err(HostError::Denied),
        }
    }
}

#[cfg_attr(test, test)]
pub fn configured_buffer_boundaries_are_enforced_before_dispatch_or_guest_lowering() {
    for (input_bytes, output_bytes, mode, expected, calls) in [
        (120, 0, 4, 0, 1),
        (121, 0, 4, 2, 0),
        (0, 124, 0, 0, 1),
        (0, 125, 0, 2, 1),
    ] {
        let binding = Arc::new(Storage {
            calls: std::sync::atomic::AtomicUsize::new(0),
            reply_bytes: output_bytes,
        });
        let context = Context {
            grants: Grants {
                prefixes: vec!["objects/".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let host = Host::with_limits(context, binding.clone(), small_limits());
        let engine = engine::Engine::default();
        let source = include_str!("limits_fixtures/buffers.wat")
            .replace("1048577", &input_bytes.to_string());
        let (mut store, instance) = host::instantiate(&engine, source, host).unwrap();
        let run = instance
            .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
            .unwrap();
        assert_eq!(run.call(&mut store, (mode,)).unwrap(), (expected,));
        assert_eq!(binding.calls.load(Ordering::SeqCst), calls);
        if expected == 2 {
            assert_eq!(
                host::outcome::classify(store.data(), Ok(())),
                host::outcome::InvocationOutcome::BufferLimit
            );
        }
    }
}

#[cfg_attr(test, test)]
pub fn guest_memory_exhaustion_is_typed_and_a_following_fixture_succeeds() {
    let binding = Arc::new(Storage {
        calls: std::sync::atomic::AtomicUsize::new(0),
        reply_bytes: 0,
    });
    let engine = engine::Engine::default();
    for mode in [2, 0] {
        let (mut store, instance) = host::instantiate(
            &engine,
            include_str!("limits_fixtures/guest-limits.wat"),
            Host::with_limits(Context::default(), binding.clone(), small_limits()),
        )
        .unwrap();
        let run = instance
            .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
            .unwrap();
        let result = run.call(&mut store, (mode,));
        let outcome = host::outcome::classify(store.data(), result.as_ref().map(|_| ()));
        assert_eq!(
            outcome,
            if mode == 2 {
                host::outcome::InvocationOutcome::GuestMemoryLimit
            } else {
                host::outcome::InvocationOutcome::Success
            }
        );
        if mode == 0 {
            assert_eq!(result.unwrap(), (42,));
        }
    }
}

#[cfg_attr(test, test)]
pub fn never_yielding_guest_obeys_the_configured_deadline() {
    let mut config = engine::Config::new();
    config.epoch_interruption(true);
    let engine = engine::Engine::new(&config).unwrap();
    let binding = Arc::new(Storage {
        calls: std::sync::atomic::AtomicUsize::new(0),
        reply_bytes: 0,
    });
    let start = std::time::Instant::now();
    let (mut store, instance) = host::instantiate(
        &engine,
        include_str!("limits_fixtures/guest-limits.wat"),
        Host::with_limits(Context::default(), binding, small_limits()),
    )
    .unwrap();
    let run = instance
        .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
        .unwrap();
    let (done, finished) = mpsc::channel();
    let watchdog = engine.clone();
    let guard = std::thread::spawn(move || {
        if finished.recv_timeout(Duration::from_secs(3)).is_err() {
            watchdog.increment_epoch();
        }
    });
    let result = run.call(&mut store, (1,));
    let elapsed = start.elapsed();
    done.send(()).unwrap();
    guard.join().unwrap();
    assert_eq!(
        host::outcome::classify(store.data(), result.as_ref().map(|_| ())),
        host::outcome::InvocationOutcome::GuestInterrupted
    );
    assert!(
        elapsed >= Duration::from_millis(100),
        "another store's early tick must not interrupt early: {elapsed:?}"
    );
    assert!(
        elapsed <= Duration::from_millis(100) + TOLERANCE,
        "guest deadline lost: {elapsed:?}"
    );
}

fn uncertain_followed_by_trap(mode: u32) {
    let (dispatched, started) = mpsc::sync_channel(1);
    let (release, released) = mpsc::sync_channel(1);
    let (committed, observed) = mpsc::sync_channel(1);
    let binding = Arc::new(DelayedMutation {
        dispatched,
        release: Mutex::new(released),
        committed,
    });
    let context = Context {
        grants: Grants {
            topics: vec!["orders".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let mut config = engine::Config::new();
    config.epoch_interruption(true);
    let engine = engine::Engine::new(&config).unwrap();
    let (mut store, instance) = host::instantiate(
        &engine,
        include_str!("limits_fixtures/delayed-mutation.wat"),
        Host::with_limits(context, binding, small_limits()),
    )
    .unwrap();
    let run = instance
        .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
        .unwrap();
    let result = run.call(&mut store, (mode,));
    started.recv_timeout(Duration::from_secs(2)).unwrap();
    release.send(()).unwrap();
    observed.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(
        result.is_err(),
        "fixture must trap after the unacknowledged mutation"
    );
    if mode == 4 {
        assert!(matches!(
            result.as_ref().unwrap_err().downcast_ref::<engine::Trap>(),
            Some(engine::Trap::Interrupt)
        ));
    }
    if mode == 5 {
        assert!(store.data().memory_limited());
    }
    assert_eq!(
        host::outcome::classify(store.data(), result.as_ref().map(|_| ())),
        host::outcome::InvocationOutcome::Uncertain
    );
}

#[cfg_attr(test, test)]
pub fn uncertain_is_not_hidden_by_a_later_epoch_interrupt() {
    uncertain_followed_by_trap(4);
}
#[cfg_attr(test, test)]
pub fn uncertain_is_not_hidden_by_a_later_memory_limit() {
    uncertain_followed_by_trap(5);
}

struct BlockedRead {
    release: Mutex<mpsc::Receiver<()>>,
}
impl Bindings for BlockedRead {
    fn call(&self, _: &Context, op: Operation) -> Result<Reply, HostError> {
        assert!(matches!(op, Operation::Read { .. }));
        self.release.lock().unwrap().recv().unwrap();
        Ok(Reply::Data(vec![42]))
    }
}
#[cfg_attr(test, test)]
pub fn blocked_read_uses_only_the_host_deadline_with_fuel_and_epochs_disabled() {
    let (release, released) = mpsc::sync_channel(1);
    let context = Context {
        grants: Grants {
            prefixes: vec!["objects/".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let binding = Arc::new(BlockedRead {
        release: Mutex::new(released),
    });
    let engine = engine::Engine::default();
    let (mut store, instance) = host::instantiate(
        &engine,
        include_str!("limits_fixtures/buffers.wat"),
        Host::with_limits(context, binding, small_limits()),
    )
    .unwrap();
    let run = instance
        .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
        .unwrap();
    let start = std::time::Instant::now();
    let result = run.call(&mut store, (0,)).unwrap();
    let elapsed = start.elapsed();
    release.send(()).unwrap();
    assert_eq!(result, (3,));
    assert!(!store.data().uncertain_host_call());
    assert_eq!(
        host::outcome::classify(store.data(), Ok(())),
        host::outcome::InvocationOutcome::Cancelled
    );
    assert!(elapsed >= Duration::from_millis(50));
    assert!(elapsed <= Duration::from_millis(50) + TOLERANCE);
}

#[cfg_attr(test, test)]
pub fn configured_storage_keys_are_bounded_before_host_copy_or_binding() {
    let binding = Arc::new(Storage {
        calls: std::sync::atomic::AtomicUsize::new(0),
        reply_bytes: 0,
    });
    let context = Context {
        grants: Grants {
            prefixes: vec!["objects/".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let source = include_str!("limits_fixtures/buffers.wat")
        .replace("objects/good", &format!("objects/{}", "k".repeat(184)))
        .replacen(
            "(local.set $len (i32.const 12))",
            "(local.set $len (i32.const 192))",
            1,
        );
    let (mut store, instance) = host::instantiate(
        &engine::Engine::default(),
        source,
        Host::with_limits(context, binding.clone(), small_limits()),
    )
    .unwrap();
    let run = instance
        .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, (0,)).unwrap(), (2,));
    assert_eq!(binding.calls.load(Ordering::SeqCst), 0);
}

#[cfg_attr(test, test)]
pub fn acknowledged_read_timeout_cannot_be_classified_as_success() {
    struct TimedOut;
    impl Bindings for TimedOut {
        fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
            Err(HostError::Timeout)
        }
    }
    let context = Context {
        grants: Grants {
            prefixes: vec!["objects/".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let (mut store, instance) = host::instantiate(
        &engine::Engine::default(),
        include_str!("limits_fixtures/buffers.wat"),
        Host::with_limits(context, Arc::new(TimedOut), small_limits()),
    )
    .unwrap();
    let run = instance
        .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, (0,)).unwrap(), (3,));
    assert_eq!(
        host::outcome::classify(store.data(), Ok(())),
        host::outcome::InvocationOutcome::Cancelled
    );
    assert!(!store.data().uncertain_host_call());
}
