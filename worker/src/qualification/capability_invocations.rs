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
pub fn execution_handles_retain_bounded_workflow_metadata() {
    let _case = crate::capability_support::serial_case();
    // Records every start; the reply's execution id has a configurable length.
    struct Workflows {
        starts: AtomicUsize,
        execution_id: usize,
    }
    impl Bindings for Workflows {
        fn call(&self, _: &Context, operation: Operation) -> Result<Reply, HostError> {
            match operation {
                Operation::Start { .. } => {
                    self.starts.fetch_add(1, Ordering::SeqCst);
                    Ok(Reply::Execution("e".repeat(self.execution_id)))
                }
                _ => Err(HostError::Timeout),
            }
        }
    }
    let engine = wasmtime::Engine::default();
    let instantiate = |execution_id: usize| {
        let binding = Arc::new(Workflows {
            starts: AtomicUsize::new(0),
            execution_id,
        });
        // Every target is granted, so only the retention bound can reject one.
        let context = Context {
            grants: Grants {
                targets: [16, 1024, 1025, host::BUFFER_BYTES - 8]
                    .iter()
                    .map(|len| "a".repeat(*len))
                    .collect(),
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
        (store, run, binding)
    };
    // A granted target that passes the per-call ceiling would otherwise be copied
    // into each of 1024 live handles: about 1 GiB outside the store limiter.
    let (mut store, run, binding) = instantiate(16);
    for target in [1025, host::BUFFER_BYTES as u32 - 8] {
        assert_eq!(run.call(&mut store, (target,)).unwrap(), (2,), "{target}");
    }
    assert_eq!(binding.starts.load(Ordering::SeqCst), 0);
    // The bound itself is accepted for every live handle, up to the handle ceiling.
    for _ in 0..1024 {
        assert_eq!(run.call(&mut store, (1024,)).unwrap(), (0,));
    }
    assert_eq!(run.call(&mut store, (1024,)).unwrap(), (2,));
    assert_eq!(binding.starts.load(Ordering::SeqCst), 1024);
    // A binding's execution id is retained metadata as well.
    let (mut store, run, _) = instantiate(1024);
    assert_eq!(run.call(&mut store, (16,)).unwrap(), (0,));
    let (mut store, run, _) = instantiate(1025);
    assert_eq!(run.call(&mut store, (16,)).unwrap(), (2,));
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

/// Live and peak heap bytes from the process-wide counting allocator. Freed memory
/// that the system allocator reuses is counted again, unlike resident memory.
#[derive(Clone, Copy, Debug)]
struct MemorySample {
    live: usize,
    peak: usize,
}

fn memory_sample() -> MemorySample {
    MemorySample {
        live: crate::counting_alloc::live(),
        peak: crate::counting_alloc::peak(),
    }
}

/// Starts a measurement: earlier peaks, such as compilation and warm-up, are discarded.
fn reset_sample() -> MemorySample {
    let live = crate::counting_alloc::reset_peak();
    MemorySample { live, peak: live }
}

/// Peak heap allocation above the live heap at the start of the measurement.
fn host_growth(before: MemorySample, after: MemorySample) -> usize {
    after.peak.saturating_sub(before.live)
}

#[cfg_attr(test, test)]
pub fn allocation_probe() {
    let _case = crate::capability_support::serial_case();
    let Ok(probe) = std::env::var("WAS02_ALLOCATION_PROBE") else {
        return;
    };
    let (mode, eager) = probe.split_once(':').unwrap();
    if eager == "control" {
        // Sensitivity at the bound: freed warm-up memory the system allocator can
        // reuse must not hide a later allocation just above the bound.
        drop(std::hint::black_box(vec![1u8; 16 * 1024 * 1024]));
        let before = reset_sample();
        drop(std::hint::black_box(vec![1u8; 2 * ALLOCATION_BOUND]));
        let after = memory_sample();
        let delta = host_growth(before, after);
        assert!(
            delta > ALLOCATION_BOUND,
            "instrumentation missed a {} byte allocation: {delta} ({before:?} -> {after:?})",
            2 * ALLOCATION_BOUND
        );
        println!("allocation-probe passed: control {delta}");
        return;
    }
    let mode: u32 = mode.parse().unwrap();
    let record = mode >= 4;
    let mode = if record { mode - 3 } else { mode };
    let engine = wasmtime::Engine::default();
    let source = if record {
        include_str!("fixtures/capability-logging.wat")
    } else {
        include_str!("fixtures/capability-events.wat")
    };
    let (before, after) = if eager == "eager" {
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
        let before = reset_sample();
        assert_eq!(run.call(&mut store, (mode,)).unwrap(), (2,));
        (before, memory_sample())
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
        let before = reset_sample();
        assert_eq!(run.call(&mut store, (mode,)).unwrap(), (2,));
        let after = memory_sample();
        assert_eq!(binding.0.load(Ordering::SeqCst), 1);
        (before, after)
    };
    let delta = host_growth(before, after);
    if eager == "eager" {
        assert!(
            delta >= CONTROL_MINIMUM,
            "negative control missed eager lift: {delta} ({before:?} -> {after:?})"
        );
    } else {
        assert!(
            delta <= ALLOCATION_BOUND,
            "host allocated {delta} bytes (bound {ALLOCATION_BOUND}; {before:?} -> {after:?})"
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
pub fn allocation_instrumentation_reports_an_allocation_above_the_bound() {
    let _case = crate::capability_support::serial_case();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["allocation_probe", "--nocapture"])
        .env("WAS02_ALLOCATION_PROBE", "0:control")
        .output()
        .unwrap();
    assert!(
        output.status.success()
            && String::from_utf8_lossy(&output.stdout).contains("allocation-probe passed: control"),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
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
    let before = reset_sample();
    // Each key fits the per-call ceiling, but retaining 64 copies must not grow
    // host memory by 64 MiB. The guest has only 2 MiB of linear memory.
    let result = run.call(&mut store, (1_048_572, 64, 0)).unwrap();
    let after = memory_sample();
    let delta = host_growth(before, after);
    assert!(
        delta <= ALLOCATION_BOUND,
        "retained host allocation: {delta} ({before:?} -> {after:?})"
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

#[cfg_attr(test, test)]
pub fn thread_local_destructor_probe() {
    if std::env::var_os("WAS02_TLS_PROBE").is_none() {
        return;
    }
    use std::sync::{Condvar, Mutex};
    use std::time::Duration;
    // Thread-local destructors cannot capture state, so the gate is process-wide.
    static RELEASE: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());
    static ENTERED: AtomicUsize = AtomicUsize::new(0);
    static EXITED: AtomicUsize = AtomicUsize::new(0);
    struct Stall;
    impl Drop for Stall {
        fn drop(&mut self) {
            ENTERED.fetch_add(1, Ordering::SeqCst);
            let (lock, wake) = &RELEASE;
            drop(
                wake.wait_while(
                    lock.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                    |released| !*released,
                )
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
            EXITED.fetch_add(1, Ordering::SeqCst);
        }
    }
    thread_local! {
        static STALL: Stall = const { Stall };
    }
    // Returns promptly, but leaves a destructor that stalls after Helper::run
    // has returned and its reply has been delivered.
    struct ThreadLocalBinding(AtomicUsize);
    impl Bindings for ThreadLocalBinding {
        fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            STALL.with(|_| {});
            Ok(Reply::Empty)
        }
    }
    let engine = wasmtime::Engine::default();
    let binding = Arc::new(ThreadLocalBinding(AtomicUsize::new(0)));
    let context = Context {
        grants: Grants {
            topics: vec!["orders".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let guest = |timeout: Duration| {
        let mut host = Host::new(context.clone(), binding.clone());
        host.set_call_timeout(timeout);
        event_guest(&engine, host)
    };
    for _ in 0..24 {
        let (mut store, run) = guest(Duration::from_secs(30));
        assert_eq!(run.call(&mut store, (0,)).unwrap(), (0,));
    }
    wait_for("all 24 thread-local destructors to stall", || {
        ENTERED.load(Ordering::SeqCst) == 24
    });
    // Every helper thread is still live, so no further helper may be admitted.
    let (mut store, run) = guest(Duration::from_millis(200));
    let result = run.call(&mut store, (0,)).unwrap();
    let calls = binding.0.load(Ordering::SeqCst);
    // Always release the destructor gate before asserting the result.
    *RELEASE.0.lock().unwrap() = true;
    RELEASE.1.notify_all();
    assert_eq!(
        (result, calls),
        ((3,), 24),
        "helpers released their permits before thread-local destructors exited"
    );
    wait_for("stalled thread-local destructors to exit", || {
        EXITED.load(Ordering::SeqCst) == ENTERED.load(Ordering::SeqCst)
    });
    // A permit becomes available once its helper thread has terminated.
    let (mut store, run) = guest(Duration::from_secs(30));
    assert_eq!(run.call(&mut store, (0,)).unwrap(), (0,));
    println!("thread-local-destructor-probe passed");
}

#[cfg_attr(test, test)]
pub fn helper_permits_cover_thread_local_destructors() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["thread_local_destructor_probe", "--nocapture"])
        .env("WAS02_TLS_PROBE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success()
            && String::from_utf8_lossy(&output.stdout)
                .contains("thread-local-destructor-probe passed"),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn limits_engine() -> wasmtime::Engine {
    let mut config = wasmtime::Config::new();
    config.epoch_interruption(true);
    wasmtime::Engine::new(&config).unwrap()
}

fn execution_limits() -> falcone_serverless_worker::limits::Limits {
    falcone_serverless_worker::limits::Limits::parse(include_str!("fixtures/limits-v1.json"))
        .unwrap()
}

#[cfg(test)]
#[test]
fn configured_guest_buffer_checks_the_entire_aggregate_before_binding() {
    guest_buffer_outcome();
}

pub fn guest_buffer_outcome() -> Option<host::ExecutionOutcome> {
    let _case = crate::capability_support::serial_case();
    let engine = limits_engine();
    let mut observed = None;
    for (payload, expected, calls) in [(50, 0, 1), (51, 2, 0)] {
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
            include_str!("fixtures/limits-events.wat"),
            Host::with_limits(context, binding.clone(), &execution_limits()),
        )
        .unwrap();
        let run = instance
            .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
            .unwrap();
        assert_eq!(run.call(&mut store, (payload,)).unwrap(), (expected,));
        assert_eq!(binding.0.load(Ordering::SeqCst), calls);
        observed = store.data().outcome();
        assert_eq!(
            observed,
            if calls == 0 {
                Some(host::ExecutionOutcome::BufferLimit)
            } else {
                None
            }
        );
    }
    observed
}

#[cfg(test)]
#[test]
fn configured_storage_reply_is_not_delivered_above_the_aggregate_limit() {
    storage_reply_outcome();
}

pub fn storage_reply_outcome() -> Option<host::ExecutionOutcome> {
    let _case = crate::capability_support::serial_case();
    struct Storage {
        calls: AtomicUsize,
        bytes: usize,
    }
    impl Bindings for Storage {
        fn call(&self, _: &Context, operation: Operation) -> Result<Reply, HostError> {
            assert!(matches!(operation, Operation::Read { .. }));
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Reply::Data(vec![42; self.bytes]))
        }
    }
    let engine = limits_engine();
    let mut observed = None;
    for (bytes, expected, delivered) in [(60, 0, 60), (61, 2, 0)] {
        let binding = Arc::new(Storage {
            calls: AtomicUsize::new(0),
            bytes,
        });
        let context = Context {
            grants: Grants {
                prefixes: vec!["objects/".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let (mut store, instance) = host::instantiate(
            &engine,
            include_str!("fixtures/limits-storage.wat"),
            Host::with_limits(
                context,
                binding.clone(),
                &falcone_serverless_worker::limits::Limits::parse(
                    &include_str!("fixtures/limits-v1.json")
                        .replace("\"guest_to_host_bytes\":64", "\"guest_to_host_bytes\":128"),
                )
                .unwrap(),
            ),
        )
        .unwrap();
        let run = instance
            .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
            .unwrap();
        assert_eq!(run.call(&mut store, (0,)).unwrap(), (expected,));
        let allocated = instance
            .get_typed_func::<(), (u32,)>(&mut store, "allocated")
            .unwrap();
        assert_eq!(allocated.call(&mut store, ()).unwrap(), (delivered,));
        assert_eq!(binding.calls.load(Ordering::SeqCst), 1);
        observed = store.data().outcome();
        assert_eq!(
            observed,
            if expected == 2 {
                Some(host::ExecutionOutcome::BufferLimit)
            } else {
                None
            }
        );
    }
    observed
}

#[cfg(test)]
#[test]
fn configured_http_reply_is_not_delivered_above_the_aggregate_limit() {
    http_reply_outcome();
}

pub fn http_reply_outcome() -> Option<host::ExecutionOutcome> {
    let _case = crate::capability_support::serial_case();
    struct Http {
        calls: AtomicUsize,
        bytes: usize,
    }
    impl Bindings for Http {
        fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
            unreachable!()
        }
        fn resolve(&self, _: &str) -> Result<Vec<std::net::IpAddr>, HostError> {
            Ok(vec!["93.184.216.34".parse().unwrap()])
        }
        fn http(
            &self,
            _: &Context,
            _: &host::net::PinnedRequest,
        ) -> Result<host::net::Response, HostError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(host::net::Response {
                status: 200,
                headers: vec![],
                body: vec![42; self.bytes],
            })
        }
    }
    let engine = limits_engine();
    let mut observed = None;
    for (bytes, expected, delivered) in [(54, 0, 54), (55, 2, 0)] {
        let binding = Arc::new(Http {
            calls: AtomicUsize::new(0),
            bytes,
        });
        let context = Context {
            grants: Grants {
                hosts: vec!["allowed.test".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let (mut store, instance) = host::instantiate(
            &engine,
            include_str!("fixtures/limits-http.wat"),
            Host::with_limits(context, binding.clone(), &execution_limits()),
        )
        .unwrap();
        let run = instance
            .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
            .unwrap();
        assert_eq!(run.call(&mut store, (0,)).unwrap(), (expected,));
        let allocated = instance
            .get_typed_func::<(), (u32,)>(&mut store, "allocated")
            .unwrap();
        assert_eq!(allocated.call(&mut store, ()).unwrap(), (delivered,));
        assert_eq!(binding.calls.load(Ordering::SeqCst), 1);
        observed = store.data().outcome();
        assert_eq!(
            observed,
            if expected == 2 {
                Some(host::ExecutionOutcome::BufferLimit)
            } else {
                None
            }
        );
    }
    observed
}

#[cfg(test)]
#[test]
fn guest_memory_denial_records_a_host_event_even_when_the_guest_recovers() {
    guest_memory_outcome();
}

pub fn guest_memory_outcome() -> Option<host::ExecutionOutcome> {
    let _case = crate::capability_support::serial_case();
    let engine = limits_engine();
    let limits = falcone_serverless_worker::limits::Limits::parse(
        &include_str!("fixtures/limits-v1.json").replace("268435456", "196608"),
    )
    .unwrap();
    let (mut store, instance) = host::instantiate(
        &engine,
        include_str!("fixtures/limits-memory.wat"),
        Host::with_limits(Context::default(), Arc::new(Recording::default()), &limits),
    )
    .unwrap();
    let run = instance
        .get_typed_func::<(u32,), (i32,)>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, (2,)).unwrap(), (-1,));
    let observed = store.data().outcome();
    assert_eq!(observed, Some(host::ExecutionOutcome::GuestMemoryLimit));
    // The worker and the existing guest survive, and a later valid fixture succeeds.
    assert_eq!(run.call(&mut store, (1,)).unwrap(), (2,));
    let (mut valid, instance) = host::instantiate(
        &engine,
        include_str!("fixtures/limits-memory.wat"),
        Host::with_limits(Context::default(), Arc::new(Recording::default()), &limits),
    )
    .unwrap();
    let run = instance
        .get_typed_func::<(u32,), (i32,)>(&mut valid, "run")
        .unwrap();
    assert_eq!(run.call(&mut valid, (1,)).unwrap(), (2,));
    assert_eq!(valid.data().outcome(), None);
    let oversized = include_str!("fixtures/limits-memory.wat").replace(" 2 16", " 4 16");
    let error = host::instantiate(
        &engine,
        oversized,
        Host::with_limits(Context::default(), Arc::new(Recording::default()), &limits),
    )
    .err()
    .expect("oversized initial memory must be rejected");
    assert!(
        format!("{error:#}").contains("memory minimum size of 4 pages exceeds memory limits"),
        "{error:#}"
    );
    observed
}

#[cfg(test)]
#[test]
fn configured_guest_deadline_interrupts_an_infinite_loop() {
    guest_deadline_outcome();
}

pub fn guest_deadline_outcome() -> Option<host::ExecutionOutcome> {
    let _case = crate::capability_support::serial_case();
    let mut config = wasmtime::Config::new();
    config.epoch_interruption(true).consume_fuel(false);
    let engine = wasmtime::Engine::new(&config).unwrap();
    let (mut store, instance) = host::instantiate(
        &engine,
        include_str!("fixtures/limits-loop.wat"),
        Host::with_limits(
            Context::default(),
            Arc::new(Recording::default()),
            &execution_limits(),
        ),
    )
    .unwrap();
    let run = instance
        .get_typed_func::<(), (i32,)>(&mut store, "run")
        .unwrap();
    let start = std::time::Instant::now();
    let error = run.call(&mut store, ()).unwrap_err();
    assert_eq!(
        error.downcast_ref::<wasmtime::Trap>(),
        Some(&wasmtime::Trap::Interrupt)
    );
    assert_eq!(
        store.data().outcome(),
        Some(host::ExecutionOutcome::GuestInterrupted)
    );
    assert!(start.elapsed() <= std::time::Duration::from_secs(3));
    store.data().outcome()
}

#[cfg(test)]
#[test]
fn host_call_deadline_alone_records_uncertain_until_binding_exit_is_observed() {
    host_call_deadline_outcome();
}

pub fn host_call_deadline_outcome() -> Option<host::ExecutionOutcome> {
    let _case = crate::capability_support::serial_case();
    use std::sync::atomic::AtomicBool;
    use std::sync::{Condvar, Mutex};
    struct Blocked {
        entered: AtomicBool,
        release: (Mutex<bool>, Condvar),
    }
    impl Bindings for Blocked {
        fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
            if self.entered.swap(true, Ordering::AcqRel) {
                return Err(HostError::Timeout);
            }
            let (lock, wake) = &self.release;
            drop(
                wake.wait_while(lock.lock().unwrap(), |released| !*released)
                    .unwrap(),
            );
            Ok(Reply::Empty)
        }
    }
    struct Release(Arc<Blocked>);
    impl Drop for Release {
        fn drop(&mut self) {
            *self.0.release.0.lock().unwrap() = true;
            self.0.release.1.notify_all();
        }
    }
    let binding = Arc::new(Blocked {
        entered: AtomicBool::new(false),
        release: (Mutex::new(false), Condvar::new()),
    });
    let release = Release(binding.clone());
    let engine = wasmtime::Engine::default();
    assert!(!engine.get_epoch_interruption() && !engine.get_consume_fuel());
    let context = Context {
        grants: Grants {
            topics: vec!["orders".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let mut host = Host::with_limits(context, binding.clone(), &execution_limits())
        .without_guest_deadline_for_qualification();
    // The public override may shorten a call, but cannot raise the validated
    // 100 ms contract to a more permissive deadline.
    host.set_call_timeout(std::time::Duration::from_millis(2500));
    let cancelled = host.cancellation();
    let (mut store, instance) =
        host::instantiate(&engine, include_str!("fixtures/limits-events.wat"), host).unwrap();
    let run = instance
        .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
        .unwrap();
    let start = std::time::Instant::now();
    let guest = std::thread::spawn(move || {
        let result = run.call(&mut store, (0,)).unwrap();
        (store, result)
    });
    wait_for("blocked binding entry", || {
        binding.entered.load(Ordering::Acquire)
    });
    let (mut store, result) = guest.join().unwrap();
    assert_eq!(result, (3,)); // WIT timeout, never success.
    assert!(start.elapsed() <= std::time::Duration::from_millis(2100));
    assert!(!cancelled.load(Ordering::Acquire));
    assert_eq!(
        store.data().outcome(),
        Some(host::ExecutionOutcome::HostCallUncertain)
    );
    // A later binding can exit after returning timeout while the first call is
    // still running. Its exit must not erase the earlier uncertain evidence.
    assert_eq!(run.call(&mut store, (0,)).unwrap(), (3,));
    let observe_until = std::time::Instant::now() + std::time::Duration::from_millis(100);
    while std::time::Instant::now() < observe_until {
        assert_eq!(
            store.data().outcome(),
            Some(host::ExecutionOutcome::HostCallUncertain)
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    drop(release);
    wait_for("binding exit observed", || {
        store.data().outcome() == Some(host::ExecutionOutcome::HostCallCancelled)
    });
    store.data().host_call_outcome()
}

// Exercise combined events through the same WIT boundary as the blocked-call fixture.
#[cfg(test)]
fn unresolved_call_with_guest_event(interrupt: bool) {
    let _case = crate::capability_support::serial_case();
    use std::sync::{Condvar, Mutex};
    struct Blocked {
        entered: std::sync::atomic::AtomicBool,
        release: (Mutex<bool>, Condvar),
    }
    impl Bindings for Blocked {
        fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
            self.entered.store(true, Ordering::Release);
            let (lock, wake) = &self.release;
            drop(
                wake.wait_while(lock.lock().unwrap(), |released| !*released)
                    .unwrap(),
            );
            Ok(Reply::Empty)
        }
    }
    struct Release(Arc<Blocked>);
    impl Drop for Release {
        fn drop(&mut self) {
            *self.0.release.0.lock().unwrap() = true;
            self.0.release.1.notify_all();
        }
    }
    let binding = Arc::new(Blocked {
        entered: Default::default(),
        release: (Mutex::new(false), Condvar::new()),
    });
    let release = Release(binding.clone());
    let context = Context {
        grants: Grants {
            topics: vec!["orders".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let engine = limits_engine();
    let (mut store, instance) = host::instantiate(
        &engine,
        include_str!("fixtures/limits-events.wat"),
        Host::with_limits(context, binding.clone(), &execution_limits()),
    )
    .unwrap();
    let run = instance
        .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
        .unwrap();
    if !interrupt {
        // The guest handles the rejected limit+1 aggregate before starting a binding.
        assert_eq!(run.call(&mut store, (51,)).unwrap(), (2,));
        assert!(!binding.entered.load(Ordering::Acquire));
        assert_eq!(
            store.data().outcome(),
            Some(host::ExecutionOutcome::BufferLimit)
        );
    }
    let guest = std::thread::spawn(move || {
        assert_eq!(run.call(&mut store, (0,)).unwrap(), (3,));
        if interrupt {
            let spin = instance
                .get_typed_func::<(), ()>(&mut store, "spin")
                .unwrap();
            let error = spin.call(&mut store, ()).unwrap_err();
            assert_eq!(
                error.downcast_ref::<wasmtime::Trap>(),
                Some(&wasmtime::Trap::Interrupt)
            );
        }
        store
    });
    wait_for("combined-event binding entry", || {
        binding.entered.load(Ordering::Acquire)
    });
    let store = guest.join().unwrap();
    assert_eq!(
        store.data().outcome(),
        Some(host::ExecutionOutcome::HostCallUncertain)
    );
    assert_eq!(
        store.data().host_call_outcome(),
        Some(host::ExecutionOutcome::HostCallUncertain)
    );
    drop(release);
    wait_for("combined-event binding exit", || {
        store.data().outcome() == Some(host::ExecutionOutcome::HostCallCancelled)
    });
}

#[cfg(test)]
#[test]
fn unresolved_call_is_not_hidden_by_guest_interruption() {
    unresolved_call_with_guest_event(true);
}

#[cfg(test)]
#[test]
fn unresolved_call_is_not_hidden_by_a_handled_buffer_limit() {
    unresolved_call_with_guest_event(false);
}

/// Trusted qualification child: guest computation deliberately ignores host cancellation.
pub fn cancellation_ignoring_child() -> ! {
    use std::io::{Read, Write};
    let engine = wasmtime::Engine::default();
    let host = Host::new(Context::default(), Arc::new(Recording::default()));
    let cancelled = host.cancellation();
    let (mut store, instance) =
        host::instantiate(&engine, include_str!("fixtures/limits-loop.wat"), host).unwrap();
    let run = instance
        .get_typed_func::<(), (i32,)>(&mut store, "run")
        .unwrap();
    std::thread::spawn(move || {
        let mut request = [0; 7];
        if std::io::stdin().read_exact(&mut request).is_ok() && request == *b"cancel\n" {
            cancelled.store(true, Ordering::Release);
            println!("cancel-received");
            std::io::stdout().flush().unwrap();
        }
    });
    println!("limits-child-ready");
    std::io::stdout().flush().unwrap();
    let _ = run.call(&mut store, ());
    // Returning would invalidate this fixture, so a surprising engine return fails closed.
    std::process::exit(1)
}

/// Hold a valid guest inside a binding across the kill, then let it finish.
pub fn valid_guest_overlapping_termination(
    action: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let _case = crate::capability_support::serial_case();
    use std::sync::{Condvar, Mutex};
    struct Gate {
        entered: std::sync::atomic::AtomicBool,
        release: (Mutex<bool>, Condvar),
    }
    impl Bindings for Gate {
        fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
            self.entered.store(true, Ordering::Release);
            let (lock, wake) = &self.release;
            drop(
                wake.wait_while(lock.lock().unwrap(), |released| !*released)
                    .unwrap(),
            );
            Ok(Reply::Empty)
        }
    }
    struct Release(Arc<Gate>);
    impl Drop for Release {
        fn drop(&mut self) {
            *self.0.release.0.lock().unwrap() = true;
            self.0.release.1.notify_all();
        }
    }
    let binding = Arc::new(Gate {
        entered: Default::default(),
        release: (Mutex::new(false), Condvar::new()),
    });
    let release = Release(binding.clone());
    let guest = std::thread::spawn(move || {
        let engine = wasmtime::Engine::default();
        let context = Context {
            grants: Grants {
                topics: vec!["orders".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let (mut store, instance) = host::instantiate(
            &engine,
            include_str!("fixtures/limits-events.wat"),
            Host::new(context, binding),
        )
        .unwrap();
        let run = instance
            .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
            .unwrap();
        run.call(&mut store, (0,)).unwrap()
    });
    wait_for("overlapping valid guest binding entry", || {
        release.0.entered.load(Ordering::Acquire)
    });
    let result = action();
    drop(release);
    let output = guest.join().map_err(|_| "overlapping guest panicked")?;
    if output != (0,) {
        return Err("overlapping valid guest failed".into());
    }
    result
}

#[cfg_attr(test, test)]
pub fn configured_storage_keys_are_checked_before_copying_or_binding() {
    let _case = crate::capability_support::serial_case();
    struct Storage(AtomicUsize);
    impl Bindings for Storage {
        fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Reply::Data(vec![]))
        }
    }
    let limits = falcone_serverless_worker::limits::Limits::parse(
        &include_str!("fixtures/limits-v1.json")
            .replace("\"guest_to_host_bytes\":64", "\"guest_to_host_bytes\":15"),
    )
    .unwrap();
    let binding = Arc::new(Storage(AtomicUsize::new(0)));
    let context = Context {
        grants: Grants {
            prefixes: vec!["objects/".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let (mut store, instance) = host::instantiate(
        &limits_engine(),
        include_str!("fixtures/limits-storage.wat"),
        Host::with_limits(context, binding.clone(), &limits),
    )
    .unwrap();
    let run = instance
        .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, (0,)).unwrap(), (2,));
    assert_eq!(binding.0.load(Ordering::SeqCst), 0);
}
