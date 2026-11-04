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
pub fn guest_table_growth_is_bounded_independently_of_linear_memory() {
    let _case = crate::capability_support::serial_case();
    let engine = wasmtime::Engine::default();
    let (mut store, instance) = host::instantiate(
        &engine,
        include_str!("fixtures/capability-table.wat"),
        Host::new(Context::default(), Arc::new(Recording::default())),
    )
    .unwrap();
    let grow = instance
        .get_typed_func::<(u32,), (i32,)>(&mut store, "grow")
        .unwrap();
    assert_eq!(grow.call(&mut store, (1,)).unwrap(), (1,));
    assert_eq!(grow.call(&mut store, (65_536,)).unwrap(), (-1,));
    assert_eq!(grow.call(&mut store, (1,)).unwrap(), (2,));
}

#[cfg_attr(test, test)]
pub fn exhausted_execution_handles_do_not_start_untracked_workflows() {
    let _case = crate::capability_support::serial_case();
    struct Workflows(AtomicUsize);
    impl Bindings for Workflows {
        fn call(&self, _: &Context, operation: Operation) -> Result<Reply, HostError> {
            match operation {
                Operation::Start { .. } => {
                    self.0.fetch_add(1, Ordering::SeqCst);
                    Ok(Reply::Execution("binding-id".into()))
                }
                Operation::Signal { .. } => Ok(Reply::Empty),
                _ => panic!("unexpected workflow operation"),
            }
        }
    }
    let engine = wasmtime::Engine::default();
    let binding = Arc::new(Workflows(AtomicUsize::new(0)));
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
    for _ in 0..1024 {
        assert_eq!(run.call(&mut store, (0,)).unwrap(), (0,));
    }
    for _ in 0..3 {
        assert_eq!(run.call(&mut store, (0,)).unwrap(), (2,));
    }
    assert_eq!(binding.0.load(Ordering::SeqCst), 1024);
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
            // Count first: a failed assertion below panics in the binding helper, which maps
            // to Denied, so the count is what proves whether the binding was reached.
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(
                (&*context.tenant, &*context.workspace),
                ("verified-tenant", "verified-workspace")
            );
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
        (5, false, 1, 0),
        (6, false, 1, 0),
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
    // Records every call before anything else and never panics: a panic in the binding helper
    // is mapped to Denied, which would let an out-of-grant start that reached the binding pass
    // as an expected denial with zero recorded starts.
    #[derive(Default)]
    struct Workflows {
        starts: std::sync::Mutex<Vec<String>>,
        signals: std::sync::Mutex<Vec<(String, String)>>,
        unexpected: AtomicUsize,
    }
    impl Bindings for Workflows {
        fn call(&self, _: &Context, operation: Operation) -> Result<Reply, HostError> {
            match operation {
                Operation::Start { workflow, .. } => {
                    self.starts.lock().unwrap().push(workflow);
                    Ok(Reply::Execution("binding-id".into()))
                }
                Operation::Signal {
                    execution, name, ..
                } => {
                    self.signals.lock().unwrap().push((execution, name));
                    Ok(Reply::Empty)
                }
                _ => {
                    self.unexpected.fetch_add(1, Ordering::SeqCst);
                    Err(HostError::Timeout)
                }
            }
        }
    }
    let engine = wasmtime::Engine::default();
    for mode in 0..4 {
        let binding = Arc::new(Workflows::default());
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
        // Only the granted target ever reaches the binding; an out-of-grant start does not.
        let expected_starts: &[&str] = if mode == 0 || mode == 2 {
            &["invoice"]
        } else {
            &[]
        };
        assert_eq!(
            *binding.starts.lock().unwrap(),
            expected_starts,
            "mode {mode}"
        );
        let expected_signals: &[(&str, &str)] = if mode == 0 {
            &[("binding-id", "tick")]
        } else {
            &[]
        };
        assert_eq!(
            binding
                .signals
                .lock()
                .unwrap()
                .iter()
                .map(|(execution, name)| (execution.as_str(), name.as_str()))
                .collect::<Vec<_>>(),
            expected_signals,
            "mode {mode}"
        );
        assert_eq!(binding.unexpected.load(Ordering::SeqCst), 0, "mode {mode}");
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

// Exercise the event capability through the same guest/host seam in every
// budget stage, without reaching into the process-wide limiter's internals.
fn event_guest(
    engine: &wasmtime::Engine,
    host: Host,
) -> (
    wasmtime::Store<Host>,
    wasmtime::component::TypedFunc<(u32,), (u32,)>,
) {
    let (mut store, instance) =
        host::instantiate(engine, include_str!("fixtures/capability-events.wat"), host).unwrap();
    let run = instance
        .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
        .unwrap();
    (store, run)
}

fn wait_for(label: &str, mut ready: impl FnMut() -> bool) {
    const SETTLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
    let deadline = std::time::Instant::now() + SETTLE_TIMEOUT;
    while !ready() {
        assert!(std::time::Instant::now() < deadline, "waiting for {label}");
        std::thread::sleep(std::time::Duration::from_millis(1));
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
            drop(
                wake.wait_while(lock.lock().unwrap(), |released| !*released)
                    .unwrap(),
            );
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
    const TOLERANCE: Duration = Duration::from_secs(2);
    const FAIL_FAST_TIMEOUT: Duration = Duration::from_secs(30);
    for invocation in 0..12 {
        let context = Context {
            grants: Grants {
                topics: vec!["orders".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let mut host = Host::new(context, binding.clone());
        host.set_call_timeout(if invocation < 4 {
            DEADLINE
        } else {
            FAIL_FAST_TIMEOUT
        });
        let (mut store, run) = event_guest(&engine, host);
        let start = Instant::now();
        assert_eq!(
            run.call(&mut store, (0,)).unwrap(),
            (if invocation < 4 { 3 } else { 2 },)
        );
        if invocation < 4 {
            assert!(start.elapsed() <= DEADLINE + TOLERANCE);
        } else {
            assert!(
                start.elapsed() < TOLERANCE,
                "exhaustion must not wait for the 30 s deadline"
            );
        }
    }
    wait_for("the four admitted helpers to enter the binding", || {
        binding.live.load(Ordering::SeqCst) == 4 && binding.peak.load(Ordering::SeqCst) == 4
    });
    assert_eq!(binding.calls.load(Ordering::SeqCst), 4);
    assert_eq!(
        binding.live.load(Ordering::SeqCst),
        4,
        "permits must survive invocation timeout and store drop"
    );
    assert_eq!(binding.peak.load(Ordering::SeqCst), 4);
    // All four detached calls belong to one stalled binding. A separate healthy
    // binding must still serve the ADR 0007 starting concurrency of 20 guests.
    let healthy = Arc::new(Recording::default());
    let start = Arc::new(std::sync::Barrier::new(21));
    let guests = (0..20)
        .map(|_| {
            let context = Context {
                grants: Grants {
                    topics: vec!["orders".into()],
                    ..Default::default()
                },
                ..Default::default()
            };
            let (mut store, run) = event_guest(&engine, Host::new(context, healthy.clone()));
            let start = start.clone();
            std::thread::spawn(move || {
                start.wait();
                (0..100)
                    .map(|_| run.call(&mut store, (0,)).unwrap().0)
                    .collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    start.wait();
    let results = guests
        .into_iter()
        .flat_map(|guest| guest.join().unwrap())
        .collect::<Vec<_>>();
    assert!(
        results.iter().all(|result| *result == 0),
        "healthy concurrent calls were denied"
    );
    assert_eq!(healthy.0.load(Ordering::SeqCst), 2000);
    *binding.release.0.lock().unwrap() = true;
    binding.release.1.notify_all();
    wait_for("stalled helpers to leave the binding", || {
        binding.live.load(Ordering::SeqCst) == 0
    });
    // A permit becomes available after the helper actually exits.
    let context = Context {
        grants: Grants {
            topics: vec!["orders".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let (mut store, run) = event_guest(&engine, Host::new(context, binding.clone()));
    wait_for("an exited helper to release its permit", || {
        match run.call(&mut store, (0,)).unwrap().0 {
            0 => true,
            2 => false,
            result => panic!("unexpected recovery result {result}"),
        }
    });
    // Concurrent stalls can occupy the active capacity as well as the reserve,
    // but may never exceed the process-wide ceiling, even after stores drop.
    let binding = Arc::new(Stalled {
        release: (Mutex::new(false), Condvar::new()),
        live: AtomicUsize::new(0),
        peak: AtomicUsize::new(0),
        calls: AtomicUsize::new(0),
    });
    let start = Arc::new(std::sync::Barrier::new(33));
    let mut cancellations = Vec::new();
    let guests = (0..32)
        .map(|_| {
            let context = Context {
                grants: Grants {
                    topics: vec!["orders".into()],
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut host = Host::new(context, binding.clone());
            host.set_call_timeout(FAIL_FAST_TIMEOUT);
            cancellations.push(host.cancellation());
            let (mut store, run) = event_guest(&engine, host);
            let start = start.clone();
            std::thread::spawn(move || {
                start.wait();
                run.call(&mut store, (0,)).unwrap().0
            })
        })
        .collect::<Vec<_>>();
    start.wait();
    // Cancel only after every available helper slot is observably in use.
    wait_for("all 24 helpers to enter the binding", || {
        binding.live.load(Ordering::SeqCst) == 24 && binding.peak.load(Ordering::SeqCst) == 24
    });
    for cancelled in cancellations {
        cancelled.store(true, Ordering::Release);
    }
    for guest in guests {
        assert!([2, 4].contains(&guest.join().unwrap()));
    }
    assert_eq!(binding.peak.load(Ordering::SeqCst), 24);
    assert_eq!(binding.live.load(Ordering::SeqCst), 24);
    assert_eq!(binding.calls.load(Ordering::SeqCst), 24);
    let healthy = Arc::new(Recording::default());
    let context = Context {
        grants: Grants {
            topics: vec!["orders".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let mut host = Host::new(context, healthy.clone());
    host.set_call_timeout(FAIL_FAST_TIMEOUT);
    let (mut store, run) = event_guest(&engine, host);
    let start = Instant::now();
    assert_eq!(
        run.call(&mut store, (0,)).unwrap(),
        (2,),
        "a completely stalled process budget must fail fast"
    );
    assert!(start.elapsed() < TOLERANCE);
    assert_eq!(healthy.0.load(Ordering::SeqCst), 0);
    *binding.release.0.lock().unwrap() = true;
    binding.release.1.notify_all();
    wait_for("stalled helpers to leave the binding", || {
        binding.live.load(Ordering::SeqCst) == 0
    });
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

#[cfg_attr(test, test)]
pub fn retained_storage_probe() {
    if std::env::var_os("WAS02_RETAINED_PROBE").is_none() {
        return;
    }
    let engine = wasmtime::Engine::default();
    let binding = Arc::new(Recording::default());
    let context = Context {
        grants: Grants {
            prefixes: vec!["objects/".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let (mut store, instance) = host::instantiate(
        &engine,
        include_str!("fixtures/capability-handles.wat"),
        Host::new(context, binding.clone()),
    )
    .unwrap();
    let run = instance
        .get_typed_func::<(u32, u32, u32), (u32,)>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, (12, 1, 1)).unwrap(), (0,));
    let before = high_water_bytes();
    // Each key fits the per-call ceiling, but retaining 64 copies must not grow
    // host memory by 64 MiB. The guest has only 2 MiB of linear memory.
    let result = run.call(&mut store, (1_048_572, 64, 0)).unwrap();
    let delta = high_water_bytes().saturating_sub(before);
    assert!(
        delta <= ALLOCATION_BOUND,
        "retained host allocation: {delta}"
    );
    assert_eq!(result, (2,));
    assert_eq!(binding.0.load(Ordering::SeqCst), 0);
    println!("retained-storage-probe passed: {delta}");
}

#[cfg_attr(test, test)]
pub fn retained_storage_memory_is_bounded() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["retained_storage_probe", "--nocapture"])
        .env("WAS02_RETAINED_PROBE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success()
            && String::from_utf8_lossy(&output.stdout).contains("retained-storage-probe passed:"),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg_attr(test, test)]
pub fn dropped_storage_handles_release_live_capacity() {
    let engine = wasmtime::Engine::default();
    let binding = Arc::new(Recording::default());
    let context = Context {
        grants: Grants {
            prefixes: vec!["objects/".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let (mut store, instance) = host::instantiate(
        &engine,
        include_str!("fixtures/capability-handles.wat"),
        Host::new(context, binding.clone()),
    )
    .unwrap();
    let run = instance
        .get_typed_func::<(u32, u32, u32), (u32,)>(&mut store, "run")
        .unwrap();
    // The 1 KiB key boundary succeeds, and drops must permit long-lived guests
    // to open more than 1024 handles over the invocation's lifetime.
    assert_eq!(run.call(&mut store, (1024, 4096, 1)).unwrap(), (0,));
    assert_eq!(run.call(&mut store, (1025, 1, 0)).unwrap(), (2,));
    assert_eq!(run.call(&mut store, (1024, 1024, 0)).unwrap(), (0,));
    assert_eq!(run.call(&mut store, (12, 1, 0)).unwrap(), (2,));
    assert_eq!(binding.0.load(Ordering::SeqCst), 0);
}

#[cfg_attr(test, test)]
pub fn structured_logging_redacts_guest_credentials_before_binding() {
    struct Logs(AtomicUsize);
    impl Bindings for Logs {
        fn call(&self, _: &Context, operation: Operation) -> Result<Reply, HostError> {
            let Operation::Log {
                message, fields, ..
            } = operation
            else {
                panic!("expected log")
            };
            assert!(message.is_empty());
            assert_eq!(fields.len(), 1);
            assert_eq!(fields[0].name, "Authorization");
            assert_eq!(fields[0].value, "<REDACTED>");
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Reply::Empty)
        }
    }
    let source = include_str!("fixtures/capability-logging.wat")
        .replacen(
            "(func (export \"run\")",
            "(data (i32.const 8192) \"Authorizationguesttoken\")\n    (func (export \"run\")",
            1,
        )
        .replace(
            "(i32.store (i32.const 4100) (i32.const 0))",
            r#"
          (i32.store (i32.const 4100) (i32.const 13))
          (i32.store (i32.const 4104) (i32.const 8205))
          (i32.store (i32.const 4108) (i32.const 10))"#,
        );
    let engine = wasmtime::Engine::default();
    let binding = Arc::new(Logs(AtomicUsize::new(0)));
    let (mut store, instance) = host::instantiate(
        &engine,
        source,
        Host::new(Context::default(), binding.clone()),
    )
    .unwrap();
    let run = instance
        .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, (0,)).unwrap(), (0,));
    assert_eq!(binding.0.load(Ordering::SeqCst), 1);
}

#[cfg_attr(test, test)]
pub fn checkpoints_support_read_write_and_progress_through_the_granted_storage_handle() {
    struct Checkpoints(AtomicUsize);
    impl Bindings for Checkpoints {
        fn call(&self, _: &Context, operation: Operation) -> Result<Reply, HostError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            match operation {
                Operation::Read { key, checkpoint } => {
                    assert!(checkpoint);
                    assert_eq!(key, "objects/good");
                    Ok(Reply::Data(vec![42, 43]))
                }
                Operation::Write {
                    key,
                    checkpoint,
                    data,
                } => {
                    assert!(checkpoint);
                    assert_eq!(key, "objects/good");
                    assert_eq!(data, [42, 43]);
                    Ok(Reply::Empty)
                }
                Operation::ProgressCheckpoint { key } => {
                    assert_eq!(key, "objects/good");
                    Ok(Reply::Empty)
                }
                _ => panic!("unexpected checkpoint operation"),
            }
        }
    }
    let engine = wasmtime::Engine::default();
    let binding = Arc::new(Checkpoints(AtomicUsize::new(0)));
    let context = Context {
        grants: Grants {
            prefixes: vec!["objects/".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let (mut store, instance) = host::instantiate(
        &engine,
        include_str!("fixtures/capability-checkpoint.wat"),
        Host::new(context, binding.clone()),
    )
    .unwrap();
    let run = instance
        .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
        .unwrap();
    for mode in 0..3 {
        assert_eq!(run.call(&mut store, (mode,)).unwrap(), (0,), "mode {mode}");
    }
    assert_eq!(binding.0.load(Ordering::SeqCst), 3);
}

#[cfg_attr(test, test)]
pub fn binding_drop_probe() {
    if std::env::var_os("WAS02_DROP_PROBE").is_none() {
        return;
    }
    use std::sync::{Condvar, Mutex};
    use std::time::{Duration, Instant};
    #[derive(Default)]
    struct State {
        release_calls: (Mutex<bool>, Condvar),
        release_drops: (Mutex<bool>, Condvar),
        calls: AtomicUsize,
        drops: AtomicUsize,
    }
    struct PanickingBinding(Arc<State>);
    impl Bindings for PanickingBinding {
        fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
            self.0.calls.fetch_add(1, Ordering::SeqCst);
            let (lock, wake) = &self.0.release_calls;
            drop(
                wake.wait_while(lock.lock().unwrap(), |released| !*released)
                    .unwrap(),
            );
            panic!("fixture binding panic");
        }
    }
    impl Drop for PanickingBinding {
        fn drop(&mut self) {
            self.0.drops.fetch_add(1, Ordering::SeqCst);
            let (lock, wake) = &self.0.release_drops;
            drop(
                wake.wait_while(lock.lock().unwrap(), |released| !*released)
                    .unwrap(),
            );
        }
    }
    let engine = wasmtime::Engine::default();
    let state = Arc::new(State::default());
    let context = Context {
        grants: Grants {
            topics: vec!["orders".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    for _ in 0..24 {
        let mut host = Host::new(context.clone(), Arc::new(PanickingBinding(state.clone())));
        host.set_call_timeout(Duration::from_millis(20));
        let (mut store, run) = event_guest(&engine, host);
        assert_eq!(run.call(&mut store, (0,)).unwrap(), (3,));
    }
    wait_for(
        "all admitted helpers to enter before releasing calls",
        || state.calls.load(Ordering::SeqCst) == 24,
    );
    *state.release_calls.0.lock().unwrap() = true;
    state.release_calls.1.notify_all();
    wait_for("all binding destructors to enter", || {
        state.drops.load(Ordering::SeqCst) == 24
    });
    let healthy = Arc::new(Recording::default());
    let mut host = Host::new(context, healthy.clone());
    host.set_call_timeout(Duration::from_secs(30));
    let (mut store, run) = event_guest(&engine, host);
    let start = Instant::now();
    let result = run.call(&mut store, (0,)).unwrap();
    let elapsed = start.elapsed();
    // Always release the destructor gates before asserting the result.
    *state.release_drops.0.lock().unwrap() = true;
    state.release_drops.1.notify_all();
    assert_eq!(
        result,
        (2,),
        "helpers released their permits before binding destructors exited"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "exhaustion waited for the call deadline"
    );
    assert_eq!(healthy.0.load(Ordering::SeqCst), 0);
    println!("binding-drop-probe passed");
}

#[cfg_attr(test, test)]
pub fn helper_permits_cover_binding_destructors_even_during_unwinding() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["binding_drop_probe", "--nocapture"])
        .env("WAS02_DROP_PROBE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success()
            && String::from_utf8_lossy(&output.stdout).contains("binding-drop-probe passed"),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
