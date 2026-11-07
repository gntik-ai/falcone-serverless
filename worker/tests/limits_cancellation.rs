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
const RACE_TEST: &str = "lost_tick_race_cannot_strand_a_never_yielding_guest";
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

/// On the first callback, after the wall-clock check found the deadline not yet reached
/// and before the callback re-arms, this store's own timer ticks once more and the wall
/// clock then passes the deadline. The old single-shot design had no tick left after this.
struct RacingTick {
    engine: wasmtime::Engine,
    clock: Arc<ManualClock>,
    fired: AtomicBool,
}

impl Probe for RacingTick {
    fn between_check_and_rearm(&self) {
        if !self.fired.swap(true, Ordering::AcqRel) {
            self.engine.increment_epoch();
            self.clock.advanced_ms.store(60_000, Ordering::Release);
        }
    }
}

fn race_child() {
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
    std::thread::scope(|scope| {
        // Another store on the shared engine, running its own never-yielding guest.
        let neighbour = scope.spawn(|| {
            let (mut store, run) = instantiate();
            let _armed = ticker.arm(&mut store, Duration::from_millis(1_500));
            let result = run.call(&mut store, ());
            host::classify(store.data(), &result)
        });
        let clock = Arc::new(ManualClock {
            base: Instant::now(),
            advanced_ms: AtomicU64::new(0),
        });
        let probe = Arc::new(RacingTick {
            engine: engine.clone(),
            clock: clock.clone(),
            fired: AtomicBool::new(false),
        });
        let (mut store, run) = instantiate();
        let armed = ticker.arm_with(&mut store, Duration::from_secs(30), clock, probe.clone());
        // Another store's tick: it reaches this store's callback before its deadline.
        engine.increment_epoch();
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

struct Denying;
impl host::Bindings for Denying {
    fn call(&self, _: &host::Context, _: host::Operation) -> Result<host::Reply, host::HostError> {
        Err(host::HostError::Denied)
    }
}

/// Deterministic interleaving on a shared engine: another store's tick reaches this
/// store's callback early, then this store's own tick lands between the wall-clock check
/// and the re-arm. The guest must still be interrupted. The body runs in a re-exec child
/// under a watchdog that never touches the epoch, so a regression fails instead of hanging.
#[test]
fn lost_tick_race_cannot_strand_a_never_yielding_guest() {
    if std::env::var_os(RACE_CHILD).is_some() {
        race_child();
        return;
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", RACE_TEST, "--nocapture", "--test-threads=1"])
        .env(RACE_CHILD, "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > WATCHDOG {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the guest was not interrupted within {WATCHDOG:?}: its deadline was lost");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut output = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut output).unwrap();
    let mut errors = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut errors).unwrap();
    assert!(
        status.success() && output.contains("RACE_OK"),
        "{status:?}\n{output}\n{errors}"
    );
}
