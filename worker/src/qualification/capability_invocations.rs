use crate::host::{self, Bindings, Context, Grants, Host, HostError, Operation, Reply};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Default)]
struct Recording(AtomicUsize);
impl Bindings for Recording {
    fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Reply::Empty)
    }
}

#[cfg_attr(test, test)]
pub fn logging_is_ungated_and_record_lists_are_lifted_lazily() {
    let _case = crate::capability_support::serial_case();
    let engine = wasmtime::Engine::default();
    let binding = Arc::new(Recording::default());
    let (mut store, instance) = host::instantiate(
        &engine,
        include_str!("fixtures/capability-logging.wat"),
        Host::new(Context::default(), binding.clone()),
    )
    .unwrap();
    let run = instance
        .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
        .unwrap();
    for (mode, expected) in [(0, 0), (1, 2), (2, 2)] {
        assert_eq!(run.call(&mut store, (mode,)).unwrap(), (expected,));
    }
    assert_eq!(binding.0.load(Ordering::SeqCst), 1);
}

#[cfg_attr(test, test)]
pub fn record_aggregate_accepts_the_ceiling_and_stops_at_first_excess() {
    let _case = crate::capability_support::serial_case();
    let engine = wasmtime::Engine::default();
    for (mode, expected) in [(4, 0), (5, 2), (6, 2)] {
        let binding = Arc::new(Recording::default());
        let (mut store, instance) = host::instantiate(
            &engine,
            include_str!("fixtures/capability-logging.wat"),
            Host::new(Context::default(), binding.clone()),
        )
        .unwrap();
        let run = instance
            .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
            .unwrap();
        assert_eq!(
            run.call(&mut store, (mode,)).unwrap(),
            (expected,),
            "mode {mode}"
        );
        assert_eq!(binding.0.load(Ordering::SeqCst), usize::from(mode == 4));
    }
}

#[cfg_attr(test, test)]
pub fn storage_keys_cannot_escape_the_verified_binding_and_buffers_are_bounded() {
    let _case = crate::capability_support::serial_case();
    struct Storage {
        calls: AtomicUsize,
        oversized_response: bool,
    }
    impl Bindings for Storage {
        fn call(&self, context: &Context, operation: Operation) -> Result<Reply, HostError> {
            assert_eq!(
                (&*context.tenant, &*context.workspace),
                ("verified-tenant", "verified-workspace")
            );
            self.calls.fetch_add(1, Ordering::SeqCst);
            match operation {
                Operation::Read { key, .. } => {
                    assert_eq!(key, "objects/good");
                    Ok(Reply::Data(if self.oversized_response {
                        vec![42; 1_048_577]
                    } else {
                        vec![42, 43]
                    }))
                }
                Operation::Write { key, data, .. } => {
                    assert_eq!(key, "objects/good");
                    assert_eq!(data, [42, 43]);
                    Ok(Reply::Empty)
                }
                _ => panic!("unexpected operation"),
            }
        }
    }
    let engine = wasmtime::Engine::default();
    for (mode, oversized_response, expected, calls) in [
        (0, false, 0, 1),
        (1, false, 0, 1),
        (2, false, 1, 0),
        (3, false, 1, 0),
        (4, false, 2, 0),
        (0, true, 2, 1),
    ] {
        let binding = Arc::new(Storage {
            calls: AtomicUsize::new(0),
            oversized_response,
        });
        let context = Context {
            tenant: "verified-tenant".into(),
            workspace: "verified-workspace".into(),
            grants: Grants {
                prefixes: vec!["objects/".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let (mut store, instance) = host::instantiate(
            &engine,
            include_str!("fixtures/capability-storage.wat"),
            Host::new(context, binding.clone()),
        )
        .unwrap();
        let run = instance
            .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
            .unwrap();
        assert_eq!(
            run.call(&mut store, (mode,)).unwrap(),
            (expected,),
            "mode {mode}"
        );
        assert_eq!(binding.calls.load(Ordering::SeqCst), calls);
    }
}

#[cfg_attr(test, test)]
pub fn workflows_authorize_targets_and_signal_only_host_issued_handles() {
    let _case = crate::capability_support::serial_case();
    struct Workflows {
        starts: AtomicUsize,
        signals: AtomicUsize,
    }
    impl Bindings for Workflows {
        fn call(&self, _: &Context, operation: Operation) -> Result<Reply, HostError> {
            match operation {
                Operation::Start { workflow, .. } => {
                    assert_eq!(workflow, "invoice");
                    self.starts.fetch_add(1, Ordering::SeqCst);
                    Ok(Reply::Execution("binding-id".into()))
                }
                Operation::Signal {
                    execution, name, ..
                } => {
                    assert_eq!(execution, "binding-id");
                    assert_eq!(name, "tick");
                    self.signals.fetch_add(1, Ordering::SeqCst);
                    Ok(Reply::Empty)
                }
                _ => panic!("unexpected operation"),
            }
        }
    }
    let engine = wasmtime::Engine::default();
    for mode in 0..4 {
        let binding = Arc::new(Workflows {
            starts: AtomicUsize::new(0),
            signals: AtomicUsize::new(0),
        });
        let context = Context {
            grants: Grants {
                targets: vec!["invoice".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let (mut store, instance) = host::instantiate(
            &engine,
            include_str!("fixtures/capability-workflows.wat"),
            Host::new(context, binding.clone()),
        )
        .unwrap();
        let run = instance
            .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
            .unwrap();
        let result = run.call(&mut store, (mode,));
        if mode < 2 {
            assert_eq!(result.unwrap(), (mode,));
        } else {
            assert!(result.is_err());
        }
        assert_eq!(
            binding.starts.load(Ordering::SeqCst),
            usize::from(mode == 0 || mode == 2)
        );
        assert_eq!(
            binding.signals.load(Ordering::SeqCst),
            usize::from(mode == 0)
        );
    }
}

#[cfg_attr(test, test)]
pub fn ungated_context_progress_and_cancellation_follow_the_world_contract() {
    let _case = crate::capability_support::serial_case();
    let engine = wasmtime::Engine::default();
    let binding = Arc::new(Recording::default());
    let context = Context {
        invocation_key: "verified-invocation".into(),
        ..Default::default()
    };
    let host = Host::new(context, binding.clone());
    let cancelled = host.cancellation();
    let (mut store, instance) = host::instantiate(
        &engine,
        include_str!("fixtures/capability-context.wat"),
        host,
    )
    .unwrap();
    let run = instance
        .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
        .unwrap();
    for (mode, expected) in [(0, 0), (1, 0), (2, 1)] {
        assert_eq!(run.call(&mut store, (mode,)).unwrap(), (expected,));
    }
    cancelled.store(true, Ordering::Release);
    assert_eq!(run.call(&mut store, (0,)).unwrap(), (1,));
    assert_eq!(binding.0.load(Ordering::SeqCst), 1);
}

#[cfg_attr(test, test)]
pub fn event_scope_and_oversized_lazy_arguments_are_checked_before_binding() {
    let _case = crate::capability_support::serial_case();
    let engine = wasmtime::Engine::default();
    let binding = Arc::new(Recording::default());
    let context = Context {
        grants: Grants {
            topics: vec!["orders".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let (mut store, instance) = host::instantiate(
        &engine,
        include_str!("fixtures/capability-events.wat"),
        Host::new(context, binding.clone()),
    )
    .unwrap();
    let run = instance
        .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
        .unwrap();
    for (mode, expected) in [(0, 0), (1, 1), (2, 2), (3, 2)] {
        assert_eq!(run.call(&mut store, (mode,)).unwrap(), (expected,));
    }
    assert_eq!(binding.0.load(Ordering::SeqCst), 1);
}

const ALLOCATION_BOUND: usize = 256 * 1024;
const CONTROL_MINIMUM: usize = 32 * 1024 * 1024;

fn high_water_bytes() -> usize {
    std::fs::read_to_string("/proc/self/status")
        .unwrap()
        .lines()
        .find_map(|line| {
            line.strip_prefix("VmHWM:").map(|value| {
                value
                    .split_whitespace()
                    .next()
                    .unwrap()
                    .parse::<usize>()
                    .unwrap()
                    * 1024
            })
        })
        .unwrap()
}

#[cfg_attr(test, test)]
pub fn allocation_probe() {
    let _case = crate::capability_support::serial_case();
    let Ok(probe) = std::env::var("WAS02_ALLOCATION_PROBE") else {
        return;
    };
    let (mode, eager) = probe.split_once(':').unwrap();
    let mode: u32 = mode.parse().unwrap();
    let record = mode >= 4;
    let mode = if record { mode - 3 } else { mode };
    let engine = wasmtime::Engine::default();
    let source = if record {
        include_str!("fixtures/capability-logging.wat")
    } else {
        include_str!("fixtures/capability-events.wat")
    };
    let delta = if eager == "eager" {
        use wasmtime::component::{Component, Linker};
        let component = Component::new(&engine, source).unwrap();
        let mut linker = Linker::<()>::new(&engine);
        if record {
            linker
                .instance("falcone:host/logging@0.1.0")
                .unwrap()
                .func_wrap(
                    "log",
                    |_, (_, message, fields): (host::Level, String, Vec<host::Pair>)| {
                        std::hint::black_box((&message, &fields));
                        let oversized = fields.len() > 1_048_576
                            || fields
                                .iter()
                                .any(|pair| pair.name.len() + pair.value.len() > 1_048_576);
                        Ok((if oversized {
                            Err(HostError::LimitExceeded)
                        } else {
                            Ok(())
                        },))
                    },
                )
                .unwrap();
        } else {
            linker
                .instance("falcone:host/events@0.1.0")
                .unwrap()
                .func_wrap("publish", |_, (destination, payload): (String, Vec<u8>)| {
                    std::hint::black_box((&destination, &payload));
                    Ok((if destination.len() + payload.len() > 1_048_576 {
                        Err(HostError::LimitExceeded)
                    } else {
                        Ok(())
                    },))
                })
                .unwrap();
        }
        let mut store = wasmtime::Store::new(&engine, ());
        store.set_hostcall_fuel(256 * 1024 * 1024 * 16);
        let instance = linker.instantiate(&mut store, &component).unwrap();
        let run = instance
            .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
            .unwrap();
        assert_eq!(run.call(&mut store, (0,)).unwrap(), (0,));
        let before = high_water_bytes();
        assert_eq!(run.call(&mut store, (mode,)).unwrap(), (2,));
        high_water_bytes().saturating_sub(before)
    } else {
        let context = Context {
            grants: Grants {
                topics: vec!["orders".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let binding = Arc::new(Recording::default());
        let (mut store, instance) =
            host::instantiate(&engine, source, Host::new(context, binding.clone())).unwrap();
        let run = instance
            .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
            .unwrap();
        assert_eq!(run.call(&mut store, (0,)).unwrap(), (0,));
        let before = high_water_bytes();
        assert_eq!(run.call(&mut store, (mode,)).unwrap(), (2,));
        let delta = high_water_bytes().saturating_sub(before);
        assert_eq!(binding.0.load(Ordering::SeqCst), 1);
        delta
    };
    if eager == "eager" {
        assert!(
            delta >= CONTROL_MINIMUM,
            "negative control missed eager lift: {delta}"
        );
    } else {
        assert!(
            delta <= ALLOCATION_BOUND,
            "host allocated {delta} bytes (bound {ALLOCATION_BOUND})"
        );
    }
    println!("allocation-probe passed: {mode} {record} {eager} {delta}");
}

#[cfg_attr(test, test)]
pub fn oversized_utf8_and_byte_list_have_bounded_host_allocation() {
    let _case = crate::capability_support::serial_case();
    // Fresh processes prevent concurrent tests and historical peaks masking a
    // regression. The guest's 192 MiB input stays on untouched zero pages.
    for mode in [2, 3, 4, 6] {
        for implementation in ["lazy", "eager"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["allocation_probe", "--nocapture"])
                .env("WAS02_ALLOCATION_PROBE", format!("{mode}:{implementation}"))
                .output()
                .unwrap();
            assert!(
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains("allocation-probe passed:"),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

#[cfg_attr(test, test)]
pub fn process_budget_probe() {
    let _case = crate::capability_support::serial_case();
    if std::env::var_os("WAS02_BUDGET_PROBE").is_none() {
        return;
    }
    use std::sync::{Condvar, Mutex};
    use std::time::{Duration, Instant};
    struct Stalled {
        release: (Mutex<bool>, Condvar),
        live: AtomicUsize,
        peak: AtomicUsize,
        calls: AtomicUsize,
    }
    impl Bindings for Stalled {
        fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let live = self.live.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(live, Ordering::SeqCst);
            let (lock, wake) = &self.release;
            let _ = wake
                .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(2), |released| {
                    !*released
                })
                .unwrap();
            self.live.fetch_sub(1, Ordering::SeqCst);
            Ok(Reply::Empty)
        }
    }
    let binding = Arc::new(Stalled {
        release: (Mutex::new(false), Condvar::new()),
        live: AtomicUsize::new(0),
        peak: AtomicUsize::new(0),
        calls: AtomicUsize::new(0),
    });
    let engine = wasmtime::Engine::default();
    const DEADLINE: Duration = Duration::from_millis(20);
    const TOLERANCE: Duration = Duration::from_millis(200);
    for invocation in 0..12 {
        let context = Context {
            grants: Grants {
                topics: vec!["orders".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let mut host = Host::new(context, binding.clone());
        host.set_call_timeout(DEADLINE);
        let (mut store, instance) = host::instantiate(
            &engine,
            include_str!("fixtures/capability-events.wat"),
            host,
        )
        .unwrap();
        let run = instance
            .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
            .unwrap();
        let start = Instant::now();
        assert_eq!(
            run.call(&mut store, (0,)).unwrap(),
            (if invocation < 4 { 3 } else { 2 },)
        );
        assert!(start.elapsed() <= DEADLINE + TOLERANCE);
        if invocation >= 4 {
            assert!(start.elapsed() < DEADLINE);
        }
    }
    assert_eq!(binding.calls.load(Ordering::SeqCst), 4);
    assert_eq!(
        binding.live.load(Ordering::SeqCst),
        4,
        "permits must survive invocation timeout and store drop"
    );
    assert_eq!(binding.peak.load(Ordering::SeqCst), 4);
    *binding.release.0.lock().unwrap() = true;
    binding.release.1.notify_all();
    let deadline = Instant::now() + TOLERANCE;
    while binding.live.load(Ordering::SeqCst) != 0 && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(binding.live.load(Ordering::SeqCst), 0);
    // A permit becomes available after the helper actually exits.
    std::thread::sleep(Duration::from_millis(10));
    let context = Context {
        grants: Grants {
            topics: vec!["orders".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let (mut store, instance) = host::instantiate(
        &engine,
        include_str!("fixtures/capability-events.wat"),
        Host::new(context, binding.clone()),
    )
    .unwrap();
    let run = instance
        .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, (0,)).unwrap(), (0,));
    println!("thread-budget-probe passed");
}

#[cfg_attr(test, test)]
pub fn helper_cap_holds_across_fresh_invocations() {
    let _case = crate::capability_support::serial_case();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["process_budget_probe", "--nocapture"])
        .env("WAS02_BUDGET_PROBE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success()
            && String::from_utf8_lossy(&output.stdout).contains("thread-budget-probe passed"),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg_attr(test, test)]
pub fn absent_grants_and_ambient_imports_fail_before_a_guest_start() {
    let _case = crate::capability_support::serial_case();
    let engine = wasmtime::Engine::default();
    for import in [
        "wasi:filesystem/preopens@0.2.0",
        "wasi:sockets/tcp@0.2.0",
        "wasi:cli/environment@0.2.0",
        "falcone:host/storage@0.1.0",
        "falcone:host/http@0.1.0",
        "falcone:host/events@0.1.0",
        "falcone:host/workflows@0.1.0",
    ] {
        let binding = Arc::new(Recording::default());
        let source = include_str!("fixtures/capability-import.wat").replace("IMPORT", import);
        let result = host::instantiate(
            &engine,
            &source,
            Host::new(Context::default(), binding.clone()),
        );
        let error = match result {
            Ok(_) => panic!("import accepted: {import}"),
            Err(error) => format!("{error:#}"),
        };
        assert!(error.contains(import), "{error}");
        assert_eq!(binding.0.load(Ordering::SeqCst), 0);
    }
    let binding = Arc::new(Recording::default());
    let source = include_str!("fixtures/capability-import.wat")
        .replace("IMPORT", "falcone:host/cancellation@0.1.0");
    host::instantiate(
        &engine,
        &source,
        Host::new(Context::default(), binding.clone()),
    )
    .unwrap();
    assert_eq!(
        binding.0.load(Ordering::SeqCst),
        1,
        "control must record its start marker"
    );
}

#[cfg_attr(test, test)]
pub fn utf16_decoding_is_bounded_by_the_configured_guest_memory() {
    let _case = crate::capability_support::serial_case();
    // Each UTF-16 code unit consumes two guest bytes and yields at most three
    // UTF-8 bytes. Latin-1 yields at most two per guest byte. Decoder capacity
    // can exceed the decoded length; both remain proportional to capped memory.
    let source = include_str!("fixtures/capability-events.wat")
        .replace(
            "(memory $memory)))",
            "(memory $memory) string-encoding=utf16))",
        )
        .replace("201326592", "2097152");
    let engine = wasmtime::Engine::default();
    let binding = Arc::new(Recording::default());
    let context = Context {
        grants: Grants {
            topics: vec!["orders".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let (mut store, instance) =
        host::instantiate(&engine, source, Host::new(context, binding.clone())).unwrap();
    let run = instance
        .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, (2,)).unwrap(), (2,));
    assert_eq!(binding.0.load(Ordering::SeqCst), 0);
    let pages = host::GUEST_MEMORY_BYTES / 65536 + 1;
    let source =
        format!("(component (core module $m (memory {pages})) (core instance (instantiate $m)))");
    let error = match host::instantiate(&engine, source, Host::new(Context::default(), binding)) {
        Ok(_) => panic!("guest memory cap bypassed"),
        Err(error) => format!("{error:#}"),
    };
    assert!(error.contains("memory"), "{error}");
}

#[cfg_attr(test, test)]
pub fn cancellation_interrupts_a_stalled_binding_without_guest_fuel_or_epoch() {
    let _case = crate::capability_support::serial_case();
    use std::{
        sync::{Mutex, mpsc},
        time::{Duration, Instant},
    };
    struct Stalled {
        entered: Mutex<mpsc::Sender<()>>,
        finished: std::sync::atomic::AtomicBool,
    }
    impl Bindings for Stalled {
        fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
            self.entered.lock().unwrap().send(()).unwrap();
            std::thread::sleep(Duration::from_millis(100));
            self.finished.store(true, Ordering::Release);
            Ok(Reply::Empty)
        }
    }
    let (send, receive) = mpsc::channel();
    let binding = Arc::new(Stalled {
        entered: Mutex::new(send),
        finished: std::sync::atomic::AtomicBool::new(false),
    });
    let context = Context {
        grants: Grants {
            topics: vec!["orders".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let host = Host::new(context, binding.clone());
    let cancel = host.cancellation();
    let engine = wasmtime::Engine::default();
    let (mut store, instance) = host::instantiate(
        &engine,
        include_str!("fixtures/capability-events.wat"),
        host,
    )
    .unwrap();
    let run = instance
        .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
        .unwrap();
    const TOLERANCE: Duration = Duration::from_millis(250);
    let canceller = std::thread::spawn(move || {
        receive.recv_timeout(TOLERANCE).unwrap();
        cancel.store(true, Ordering::Release);
    });
    let start = Instant::now();
    assert_eq!(run.call(&mut store, (0,)).unwrap(), (4,));
    assert!(start.elapsed() < TOLERANCE);
    canceller.join().unwrap();
    while !binding.finished.load(Ordering::Acquire) && start.elapsed() < TOLERANCE {
        std::thread::yield_now();
    }
    assert!(binding.finished.load(Ordering::Acquire));
}
