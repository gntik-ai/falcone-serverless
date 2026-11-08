//! WAS-02 ac3 behaviour: buffer, memory, deadline, host-call and termination limits, driven
//! through the same fixture checks the cluster pod runs. Compile limits run the trusted child.
#![cfg(target_os = "linux")]

#[path = "../src/artifact/mod.rs"]
pub mod artifact;
#[path = "../src/cache/mod.rs"]
pub mod cache;
#[path = "../src/qualification/compile_cache_fixtures.rs"]
pub mod compile_cache_fixtures;
#[path = "../src/qualification/limits_fixtures.rs"]
mod limits_fixtures;

use falcone_serverless_worker::host::{self, Clock, EpochClock, Outcome, Probe};
use falcone_serverless_worker::limits::{Field, Limits};
use limits_fixtures::{Check, Fail, Fixtures, Setup, Verdict};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

const RACE_CHILD: &str = "WAS02_RACE_CHILD";
const RACE_TEST: &str = "race_scenario_child";
/// Far above the few milliseconds a healthy child needs; only a stranded guest reaches it.
const WATCHDOG: Duration = Duration::from_secs(60);

fn fixtures() -> Fixtures {
    Fixtures {
        complete: include_str!("fixtures/was-02/complete.wat").into(),
        infinite_loop: include_str!("fixtures/was-02/infinite-loop.wat").into(),
        memory_exhaustion: include_str!("fixtures/was-02/memory-exhaustion.wat").into(),
        storage_io: include_str!("fixtures/was-02/storage-io.wat").into(),
    }
}

struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("was02-limits-{}-{id}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn setup(scratch: &Scratch) -> Setup {
    Setup {
        fixtures: fixtures(),
        limits: Limits::qualification(),
        compile_child: Some(env!("CARGO_BIN_EXE_artifact-compile-child").into()),
        termination_child: Some(env!("CARGO_BIN_EXE_compile-cache-qualification").into()),
        scratch: scratch.0.clone(),
    }
}

fn passes(check: fn(&Setup) -> Check) {
    let scratch = Scratch::new();
    assert_eq!(check(&setup(&scratch)), Ok(()));
}

#[test]
fn compiler_cpu_exhaustion_yields_a_typed_error_leaves_no_entry_and_the_next_compile_works() {
    passes(limits_fixtures::compile_cpu_limit);
}

#[test]
fn compiler_memory_exhaustion_yields_a_typed_error_leaves_no_entry_and_the_next_compile_works() {
    passes(limits_fixtures::compile_memory_limit);
}

#[test]
fn guest_memory_exhaustion_is_a_limit_outcome_and_the_next_fixture_runs() {
    passes(limits_fixtures::guest_memory_limit);
}

#[test]
fn guest_to_host_payload_is_accepted_at_the_limit_and_rejected_before_the_binding_one_byte_over() {
    passes(limits_fixtures::guest_to_host_buffer);
}

#[test]
fn host_to_guest_result_is_accepted_at_the_limit_and_rejected_one_byte_over() {
    passes(limits_fixtures::host_to_guest_buffer);
}

#[test]
fn the_infinite_loop_is_interrupted_within_deadline_plus_tolerance_beside_another_store() {
    passes(limits_fixtures::guest_deadline_shared_engine);
}

#[test]
fn a_blocked_host_call_ends_at_its_own_deadline_or_on_cancel_and_never_reads_as_success() {
    passes(limits_fixtures::host_call_deadline);
}

#[test]
fn an_execution_that_ignores_cancellation_is_terminated_within_budget_beside_a_valid_fixture() {
    passes(limits_fixtures::termination_fallback);
}

#[test]
fn the_runner_reports_every_fixture_passed_and_missing_helpers_unverified() {
    let scratch = Scratch::new();
    let reports = limits_fixtures::run_all(&setup(&scratch));
    assert_eq!(reports.len(), limits_fixtures::CHECKS.len());
    for report in &reports {
        assert_eq!(report.verdict, Verdict::Passed, "{}", report.name);
    }
    let mut bare = setup(&scratch);
    bare.compile_child = None;
    bare.termination_child = None;
    let unverified: Vec<_> = limits_fixtures::run_all(&bare)
        .into_iter()
        .filter(|report| matches!(report.verdict, Verdict::Unverified(_)))
        .map(|report| report.name)
        .collect();
    assert_eq!(
        unverified,
        [
            "compile_cpu_limit",
            "compile_memory_limit",
            "termination_fallback"
        ]
    );
}

#[test]
fn fixtures_parse_from_the_pod_environment_and_reject_a_missing_one() {
    let document = serde_json::json!({
        "complete": "c", "infinite_loop": "i", "memory_exhaustion": "m", "storage_io": "s"
    });
    let parsed = Fixtures::from_json(&document.to_string()).unwrap();
    assert_eq!(
        (parsed.complete, parsed.storage_io),
        ("c".to_owned(), "s".to_owned())
    );
    let mut missing = document;
    missing.as_object_mut().unwrap().remove("infinite_loop");
    assert_eq!(
        Fixtures::from_json(&missing.to_string()).err().as_deref(),
        Some("fixture infinite_loop missing")
    );
    assert_eq!(
        limits_fixtures::termination_fallback(&Setup {
            termination_child: None,
            ..setup(&Scratch::new())
        }),
        Err(Fail::Unverified("no termination child executable".into()))
    );
}

#[test]
fn an_unknown_termination_child_mode_fails_closed() {
    // FS_WAS02_CHILD_FIXTURE is set only by the supervisor, never in the test environment.
    assert_eq!(
        format!("{:?}", limits_fixtures::child_main("bogus")),
        format!("{:?}", std::process::ExitCode::FAILURE)
    );
}

#[test]
fn invocation_context_results_are_checked_against_the_host_to_guest_limit() {
    // Asymmetric limits: a key past 64 bytes of results fits guest-to-host (1024) but not host-to-guest (64).
    let limits = Limits::qualification()
        .replace(Field::GuestToHostBufferBytes, 1024)
        .unwrap()
        .replace(Field::HostToGuestBufferBytes, 64)
        .unwrap();
    let engine = wasmtime::Engine::default();
    let wat = fixtures().complete;
    let with_key = |len: usize| {
        let context = host::Context {
            invocation_key: "k".repeat(len),
            ..host::Context::default()
        };
        host::instantiate(
            &engine,
            wat.as_str(),
            host::Host::with_limits(context, Arc::new(Denying), limits),
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
    };
    // Three values with a 4-byte prefix each (the other two empty) plus 52 key bytes
    // reach 64 exactly.
    assert_eq!(with_key(52), Ok(()));
    let error = with_key(53).unwrap_err();
    assert!(error.contains("limit-exceeded"), "{error}");
}

/// Counts binding calls; every write succeeds.
struct Counting(std::sync::atomic::AtomicUsize);
impl host::Bindings for Counting {
    fn call(&self, _: &host::Context, op: host::Operation) -> Result<host::Reply, host::HostError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        match op {
            host::Operation::Write { .. } => Ok(host::Reply::Empty),
            _ => Err(host::HostError::Denied),
        }
    }
}

/// The key given to `open-object` / `open-checkpoint` is a guest-to-host string, so it counts
/// (4-byte prefix plus its bytes) against the contract's limit as any other string does.
#[test]
fn opening_a_storage_key_is_checked_against_the_guest_to_host_limit() {
    let engine = wasmtime::Engine::default();
    let objects = fixtures().storage_io;
    let checkpoints = include_str!("../src/qualification/fixtures/capability-checkpoint.wat");
    // "objects/good" is 12 bytes: 16 with its prefix. The write that follows needs 8 or 10.
    for (wat, call_args, failed) in [(objects.as_str(), 0u32, 100u32), (checkpoints, 1, 99)] {
        for (limit, expected, binding_calls) in [(16, 0, 1), (15, failed, 0)] {
            let limits = Limits::qualification()
                .replace(Field::GuestToHostBufferBytes, limit)
                .unwrap();
            let context = host::Context {
                grants: host::Grants {
                    prefixes: vec!["objects/".into()],
                    ..Default::default()
                },
                ..host::Context::default()
            };
            let binding = Arc::new(Counting(std::sync::atomic::AtomicUsize::new(0)));
            let (mut store, instance) = host::instantiate(
                &engine,
                wat,
                host::Host::with_limits(context, binding.clone(), limits),
            )
            .unwrap();
            let reply = if wat == objects.as_str() {
                let run = instance
                    .get_typed_func::<(u32,), (u32,)>(&mut store, "write")
                    .unwrap();
                run.call(&mut store, (call_args,)).unwrap().0
            } else {
                let run = instance
                    .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
                    .unwrap();
                run.call(&mut store, (call_args,)).unwrap().0
            };
            assert_eq!(
                (reply, binding.0.load(Ordering::SeqCst)),
                (expected, binding_calls),
                "limit {limit}"
            );
        }
    }
}

/// A wall clock the test advances by hand.
struct ManualClock {
    base: Instant,
    advanced_ms: AtomicU64,
}

impl Clock for ManualClock {
    fn now(&self) -> Instant {
        self.base + Duration::from_millis(self.advanced_ms.load(Ordering::Acquire))
    }
}

/// Which deadline design the race scenario runs against.
#[derive(Clone, Copy, PartialEq)]
enum Design {
    /// The shipped design: one recurring ticker while any registration is pending.
    Recurring,
    /// Negative control, the superseded design: each store has one single-shot timer.
    SingleShot,
}

/// The neighbour is interrupted this soon after it arms.
const NEIGHBOUR_DEADLINE: Duration = Duration::from_millis(200);
/// The target's own single-shot timer in the control; it never fires inside the window,
/// because the probe stands in for it.
const TARGET_TIMER: Duration = Duration::from_secs(3_600);
/// The control must still be stranded this long after the injection.
const STRAND_WINDOW: Duration = Duration::from_secs(5);

/// The first callback of the target store is the early "another store's tick". The probe
/// waits until the neighbour's guest has ended, which consumes the neighbour's final tick
/// (in the control, its only one). Then, between the wall-clock check and the re-arm, the
/// target's own timer ticks once and the wall clock passes the deadline. No tick from any
/// other store can follow, so only a design that keeps ticking can still interrupt.
struct RacingTick {
    engine: wasmtime::Engine,
    clock: Arc<ManualClock>,
    neighbour_done: Arc<AtomicBool>,
    fired: AtomicBool,
}

impl Probe for RacingTick {
    fn between_check_and_rearm(&self) {
        if self.fired.swap(true, Ordering::AcqRel) {
            return;
        }
        let waiting = Instant::now();
        while !self.neighbour_done.load(Ordering::Acquire) && waiting.elapsed() < WATCHDOG {
            std::thread::sleep(Duration::from_millis(1));
        }
        let neighbour_done = self.neighbour_done.load(Ordering::Acquire);
        self.engine.increment_epoch();
        self.clock.advanced_ms.store(60_000, Ordering::Release);
        println!("RACE_INJECTED neighbour_done={neighbour_done}");
    }
}

/// The superseded arming: a single timer tick at `timer`, and a callback that re-arms
/// whenever the wall clock has not reached the limit yet.
fn arm_single_shot(
    engine: &wasmtime::Engine,
    store: &mut wasmtime::Store<host::Host>,
    deadline: Duration,
    timer: Duration,
    clock: Arc<dyn Clock>,
    probe: Arc<dyn Probe>,
) {
    let limit = clock.now() + deadline;
    let ticking = engine.clone();
    std::thread::spawn(move || {
        std::thread::sleep(timer);
        ticking.increment_epoch();
    });
    store.epoch_deadline_callback(move |_| {
        Ok(if clock.now() >= limit {
            wasmtime::UpdateDeadline::Interrupt
        } else {
            probe.between_check_and_rearm();
            wasmtime::UpdateDeadline::Continue(1)
        })
    });
    store.set_epoch_deadline(1);
}

struct NoProbe;
impl Probe for NoProbe {
    fn between_check_and_rearm(&self) {}
}

fn race_child(design: Design) {
    let fixtures = fixtures();
    let limits = Limits::qualification()
        .replace(Field::GuestDeadlineMs, 30_000)
        .unwrap();
    let engine = host::epoch_engine().unwrap();
    let ticker = EpochClock::start(&engine).unwrap();
    let wat = fixtures.infinite_loop.as_str();
    let instantiate = || {
        let host = host::Host::with_limits(host::Context::default(), Arc::new(Denying), limits);
        let (mut store, instance) = host::instantiate(&engine, wat, host).unwrap();
        let run = instance
            .get_typed_func::<(), ()>(&mut store, "run")
            .unwrap();
        (store, run)
    };
    let ready = std::sync::Barrier::new(2);
    let neighbour_done = Arc::new(AtomicBool::new(false));
    let clock = Arc::new(ManualClock {
        base: Instant::now(),
        advanced_ms: AtomicU64::new(0),
    });
    let probe = Arc::new(RacingTick {
        engine: engine.clone(),
        clock: clock.clone(),
        neighbour_done: neighbour_done.clone(),
        fired: AtomicBool::new(false),
    });
    std::thread::scope(|scope| {
        // Another store on the shared engine, running its own never-yielding guest.
        let neighbour = scope.spawn(|| {
            let (mut store, run) = instantiate();
            let _armed = match design {
                Design::Recurring => Some(ticker.arm(&mut store, NEIGHBOUR_DEADLINE)),
                Design::SingleShot => {
                    arm_single_shot(
                        &engine,
                        &mut store,
                        NEIGHBOUR_DEADLINE,
                        NEIGHBOUR_DEADLINE,
                        Arc::new(host::SystemClock),
                        Arc::new(NoProbe),
                    );
                    None
                }
            };
            ready.wait();
            let result = run.call(&mut store, ());
            let outcome = host::classify(store.data(), &result);
            neighbour_done.store(true, Ordering::Release);
            outcome
        });
        let (mut store, run) = instantiate();
        let armed = match design {
            Design::Recurring => Some(ticker.arm_with(
                &mut store,
                Duration::from_secs(30),
                clock.clone(),
                probe.clone(),
            )),
            Design::SingleShot => {
                arm_single_shot(
                    &engine,
                    &mut store,
                    Duration::from_secs(30),
                    TARGET_TIMER,
                    clock.clone(),
                    probe.clone(),
                );
                None
            }
        };
        // Both stores are armed before either guest runs, so the neighbour's tick lands
        // while the target is already inside its guest.
        ready.wait();
        let started = Instant::now();
        let result = run.call(&mut store, ());
        assert_eq!(
            host::classify(store.data(), &result),
            Outcome::GuestInterrupted
        );
        assert!(
            probe.fired.load(Ordering::Acquire),
            "the racing tick never ran"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        drop(armed);
        assert_eq!(neighbour.join().unwrap(), Outcome::GuestInterrupted);
    });
    assert_eq!(ticker.pending(), 0);
    println!("RACE_OK");
}

/// A guard outlives its clock: the registration must keep the ticker running, so the
/// never-yielding guest is still interrupted.
fn clock_dropped_child() {
    let fixtures = fixtures();
    let limits = Limits::qualification();
    let engine = host::epoch_engine().unwrap();
    let ticker = EpochClock::start(&engine).unwrap();
    let host = host::Host::with_limits(host::Context::default(), Arc::new(Denying), limits);
    let (mut store, instance) = host::instantiate(&engine, &fixtures.infinite_loop, host).unwrap();
    let run = instance
        .get_typed_func::<(), ()>(&mut store, "run")
        .unwrap();
    let guard = ticker.arm(&mut store, Duration::from_millis(200));
    drop(ticker);
    let started = Instant::now();
    let result = run.call(&mut store, ());
    assert_eq!(
        host::classify(store.data(), &result),
        Outcome::GuestInterrupted
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    drop(guard);
    println!("CLOCK_DROPPED_OK");
}

/// Entry point of the re-exec child. It does nothing outside the parent tests below.
#[test]
fn race_scenario_child() {
    match std::env::var(RACE_CHILD).as_deref() {
        Ok("recurring") => race_child(Design::Recurring),
        Ok("single-shot") => race_child(Design::SingleShot),
        Ok("clock-dropped") => clock_dropped_child(),
        _ => {}
    }
}

/// Runs the scenario in a re-exec child. The watchdog never touches the epoch. It kills
/// the child at `limit` and reports whether the child was still running then, plus its
/// output.
fn run_race_child(design: &str, limit: Duration) -> (Option<std::process::ExitStatus>, String) {
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", RACE_TEST, "--nocapture", "--test-threads=1"])
        .env(RACE_CHILD, design)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if started.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut output = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut output).unwrap();
    let mut errors = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut errors).unwrap();
    (status, format!("{output}\n{errors}"))
}

/// Deterministic interleaving on a shared engine: another store's tick reaches the
/// target's callback early and ends that store, then the target's own tick lands between
/// the wall-clock check and the re-arm. The guest must still be interrupted. A regression
/// fails at the watchdog instead of hanging.
#[test]
fn lost_tick_race_cannot_strand_a_never_yielding_guest() {
    let (status, output) = run_race_child("recurring", WATCHDOG);
    let Some(status) = status else {
        panic!(
            "the guest was not interrupted within {WATCHDOG:?}: its deadline was lost\n{output}"
        );
    };
    assert!(
        status.success()
            && output.contains("RACE_OK")
            && output.contains("RACE_INJECTED neighbour_done=true"),
        "{status:?}\n{output}"
    );
}

/// The ticker belongs to the pending registrations, not to the clock that created it:
/// dropping the clock while a guard is held must not strand the guest.
#[test]
fn a_pending_deadline_keeps_the_ticker_alive_after_the_clock_is_dropped() {
    let (status, output) = run_race_child("clock-dropped", WATCHDOG);
    let Some(status) = status else {
        panic!("the guest was not interrupted within {WATCHDOG:?}: the ticker stopped\n{output}");
    };
    assert!(
        status.success() && output.contains("CLOCK_DROPPED_OK"),
        "{status:?}\n{output}"
    );
}

/// Negative control: the same interleaving strands a guest under the superseded
/// single-shot design, so the regression above is sensitive to the reported defect.
#[test]
fn the_same_interleaving_strands_a_guest_under_single_shot_timers() {
    let (status, output) = run_race_child("single-shot", STRAND_WINDOW);
    assert!(
        output.contains("RACE_INJECTED neighbour_done=true"),
        "the race was not injected\n{output}"
    );
    assert!(
        status.is_none() && !output.contains("RACE_OK"),
        "the guest was interrupted although no tick followed the re-arm: {status:?}\n{output}"
    );
}

struct Denying;
impl host::Bindings for Denying {
    fn call(&self, _: &host::Context, _: host::Operation) -> Result<host::Reply, host::HostError> {
        Err(host::HostError::Denied)
    }
}
