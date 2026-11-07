//! WAS-02 ac3 limit and cancellation fixtures. Every check ends passed, failed or
//! unverified; a check that cannot run says so and is never counted as a pass.
//!
//! The guests are WAT text handed in by the caller and compiled with the locked
//! toolchain at run time. No precompiled artifact is read or loaded here.
use crate::artifact::supervisor::CompileLimits;
use crate::cache::{self, Cache};
use crate::compile_cache_fixtures as compile;
use falcone_serverless_worker::host::{
    self, Bindings, Context, Ended, EpochClock, Execution, Grants, Host, HostError, Operation,
    Outcome, Reply, TerminationFallback,
};
use falcone_serverless_worker::limits::{Field, Limits};
use std::ffi::OsString;
use std::io::{self, BufRead, Read};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// The environment variable that carries the guest source to a termination child.
pub const FIXTURE_ENV: &str = "FS_WAS02_CHILD_FIXTURE";

/// Canonical-ABI bytes the host counts around a payload: a `write` is a 4-byte handle
/// overhead plus a 4-byte list header, a `read` result a 4-byte list header.
pub const WRITE_OVERHEAD: usize = 8;
pub const READ_OVERHEAD: usize = 4;

pub struct Fixtures {
    pub complete: String,
    pub infinite_loop: String,
    pub memory_exhaustion: String,
    pub storage_io: String,
}

impl Fixtures {
    /// Parse the JSON object `{"complete": "<wat>", ...}` a cluster pod receives.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let document: serde_json::Value =
            serde_json::from_str(text).map_err(|_| "fixtures are not JSON".to_owned())?;
        let get = |name: &str| {
            document[name]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("fixture {name} missing"))
        };
        Ok(Self {
            complete: get("complete")?,
            infinite_loop: get("infinite_loop")?,
            memory_exhaustion: get("memory_exhaustion")?,
            storage_io: get("storage_io")?,
        })
    }
}

pub struct Setup {
    pub fixtures: Fixtures,
    pub limits: Limits,
    /// The trusted compile child; compile-limit checks are unverified without it.
    pub compile_child: Option<PathBuf>,
    /// The executable that serves `--limits-child`; the termination check is unverified without it.
    pub termination_child: Option<PathBuf>,
    pub scratch: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Fail {
    Failed(String),
    Unverified(String),
}

pub type Check = Result<(), Fail>;

fn failed(reason: impl Into<String>) -> Fail {
    Fail::Failed(reason.into())
}

fn ensure(ok: bool, reason: &str) -> Check {
    if ok { Ok(()) } else { Err(failed(reason)) }
}

fn engine_error(error: impl std::fmt::Display) -> Fail {
    failed(format!("engine: {error}"))
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Passed,
    Failed(String),
    Unverified(String),
}

#[derive(Debug, PartialEq, Eq)]
pub struct Report {
    pub name: &'static str,
    pub verdict: Verdict,
}

pub type CheckFn = fn(&Setup) -> Check;

pub const CHECKS: [(&str, CheckFn); 8] = [
    ("compile_cpu_limit", compile_cpu_limit),
    ("compile_memory_limit", compile_memory_limit),
    ("guest_memory_limit", guest_memory_limit),
    ("guest_to_host_buffer", guest_to_host_buffer),
    ("host_to_guest_buffer", host_to_guest_buffer),
    ("guest_deadline_shared_engine", guest_deadline_shared_engine),
    ("host_call_deadline_and_cancellation", host_call_deadline),
    ("termination_fallback", termination_fallback),
];

pub fn run_all(setup: &Setup) -> Vec<Report> {
    CHECKS
        .iter()
        .map(|(name, check)| Report {
            name,
            verdict: match check(setup) {
                Ok(()) => Verdict::Passed,
                Err(Fail::Failed(reason)) => Verdict::Failed(reason),
                Err(Fail::Unverified(reason)) => Verdict::Unverified(reason),
            },
        })
        .collect()
}

/// Timing checks share the CPU with the rest of the suite; run one at a time.
fn serial() -> MutexGuard<'static, ()> {
    static CASE: Mutex<()> = Mutex::new(());
    CASE.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn replace(limits: &Limits, field: Field, value: u64) -> Result<Limits, Fail> {
    limits
        .replace(field, value)
        .map_err(|e| failed(e.to_string()))
}

fn granted() -> Context {
    Context {
        tenant: "verified-tenant".into(),
        workspace: "verified-workspace".into(),
        grants: Grants {
            prefixes: vec!["objects/".into()],
            ..Default::default()
        },
        ..Default::default()
    }
}

struct Denying;
impl Bindings for Denying {
    fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
        Err(HostError::Denied)
    }
}

type Guest = (wasmtime::Store<Host>, wasmtime::component::Instance);

fn build(
    engine: &wasmtime::Engine,
    wat: &str,
    limits: &Limits,
    bindings: Arc<dyn Bindings>,
) -> Result<Guest, Fail> {
    host::instantiate(engine, wat, Host::with_limits(granted(), bindings, *limits))
        .map_err(engine_error)
}

/// A valid fixture runs to completion on a fresh store after any failure before it.
fn control(setup: &Setup, engine: &wasmtime::Engine) -> Check {
    let (mut store, instance) = build(
        engine,
        &setup.fixtures.complete,
        &setup.limits,
        Arc::new(Denying),
    )?;
    let run = instance
        .get_typed_func::<(), (u32,)>(&mut store, "run")
        .map_err(engine_error)?;
    let result = run.call(&mut store, ());
    ensure(
        matches!(result, Ok((42,))) && host::classify(store.data(), &result) == Outcome::Completed,
        "the valid control fixture did not complete",
    )
}

fn compile_limit(
    setup: &Setup,
    name: &str,
    bytes: &[u8],
    limits: &Limits,
    diagnostic: &str,
) -> Check {
    let Some(child) = &setup.compile_child else {
        return Err(Fail::Unverified(
            "no trusted compile child executable".into(),
        ));
    };
    let directory = setup.scratch.join(name);
    std::fs::create_dir_all(&directory).map_err(|_| failed("scratch directory"))?;
    let mut cache = Cache::new(&directory, child).map_err(|e| failed(e.code))?;
    let compile_limits = |limits: &Limits| CompileLimits {
        input_bytes: 64 * 1024 * 1024,
        cpu_seconds: limits.compiler_cpu_seconds(),
        memory_bytes: limits.compiler_memory_bytes(),
        wall_clock_deadline: Instant::now() + Duration::from_secs(300),
    };
    let result = cache.compile(3, bytes, compile_limits(limits), &AtomicBool::new(false));
    ensure(
        cache::verdict(&result) == ("rejected", Some(diagnostic)),
        "the exhausting compile did not yield the typed compile-limit error",
    )?;
    let entries = std::fs::read_dir(&directory)
        .map_err(|_| failed("scratch read"))?
        .count();
    ensure(
        entries == 0,
        "a failed compile left an artifact or cache entry",
    )?;
    ensure(
        cache
            .last_child_status()
            .map_err(|_| failed("child status"))?
            .is_some(),
        "the compile child was not reaped",
    )?;
    let next = cache
        .compile(
            3,
            &compile::small(),
            compile_limits(&setup.limits),
            &AtomicBool::new(false),
        )
        .map_err(|e| failed(e.code))?;
    ensure(
        !next.cache_hit && cache.spawn_count() == 2,
        "the next valid fixture did not compile",
    )
}

pub fn compile_cpu_limit(setup: &Setup) -> Check {
    let limits = replace(&setup.limits, Field::CompilerCpuSeconds, 1)?;
    let limits = replace(&limits, Field::CompilerMemoryBytes, 1024 * 1024 * 1024)?;
    compile_limit(
        setup,
        "cpu",
        &compile::cpu(),
        &limits,
        "compile_cpu_limit_exceeded",
    )
}

pub fn compile_memory_limit(setup: &Setup) -> Check {
    let limits = replace(&setup.limits, Field::CompilerCpuSeconds, 60)?;
    let limits = replace(&limits, Field::CompilerMemoryBytes, 128 * 1024 * 1024)?;
    compile_limit(
        setup,
        "memory",
        &compile::memory(),
        &limits,
        "compile_memory_limit_exceeded",
    )
}

pub fn guest_memory_limit(setup: &Setup) -> Check {
    let limits = replace(&setup.limits, Field::GuestMemoryBytes, 16 * 1024 * 1024)?;
    let engine = wasmtime::Engine::default();
    let (mut store, instance) = build(
        &engine,
        &setup.fixtures.memory_exhaustion,
        &limits,
        Arc::new(Denying),
    )?;
    let run = instance
        .get_typed_func::<(), ()>(&mut store, "run")
        .map_err(engine_error)?;
    let result = run.call(&mut store, ());
    ensure(
        result.is_err() && host::classify(store.data(), &result) == Outcome::GuestMemoryLimit,
        "memory exhaustion did not end with the guest-memory-limit outcome",
    )?;
    drop(store);
    // The worker survived: the next fixture in the same process succeeds.
    control(setup, &engine)
}

/// Counts every binding call; replies with `reply` bytes to a read.
struct Recording {
    calls: AtomicUsize,
    reply: usize,
}
impl Bindings for Recording {
    fn call(&self, _: &Context, operation: Operation) -> Result<Reply, HostError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match operation {
            Operation::Write { .. } => Ok(Reply::Empty),
            Operation::Read { .. } => Ok(Reply::Data(vec![7; self.reply])),
            _ => Err(HostError::Denied),
        }
    }
}

const LIMIT_EXCEEDED: u32 = 2;

pub fn guest_to_host_buffer(setup: &Setup) -> Check {
    let limit = 4096;
    let limits = replace(&setup.limits, Field::GuestToHostBufferBytes, limit as u64)?;
    let engine = wasmtime::Engine::default();
    // Exactly at the limit is accepted; one byte over never reaches the binding.
    for (payload, code, calls) in [
        (limit - WRITE_OVERHEAD, 0, 1),
        (limit - WRITE_OVERHEAD + 1, LIMIT_EXCEEDED, 0),
    ] {
        let binding = Arc::new(Recording {
            calls: AtomicUsize::new(0),
            reply: 0,
        });
        let (mut store, instance) = build(
            &engine,
            &setup.fixtures.storage_io,
            &limits,
            binding.clone(),
        )?;
        let write = instance
            .get_typed_func::<(u32,), (u32,)>(&mut store, "write")
            .map_err(engine_error)?;
        let reply = write
            .call(&mut store, (payload as u32,))
            .map_err(engine_error)?;
        ensure(
            reply == (code,) && binding.calls.load(Ordering::SeqCst) == calls,
            "guest-to-host buffer boundary or binding call count is wrong",
        )?;
    }
    Ok(())
}

pub fn host_to_guest_buffer(setup: &Setup) -> Check {
    let limit = 1024;
    let limits = replace(&setup.limits, Field::HostToGuestBufferBytes, limit as u64)?;
    let engine = wasmtime::Engine::default();
    for (payload, code) in [
        (limit - READ_OVERHEAD, 0),
        (limit - READ_OVERHEAD + 1, LIMIT_EXCEEDED),
    ] {
        let binding = Arc::new(Recording {
            calls: AtomicUsize::new(0),
            reply: payload,
        });
        let (mut store, instance) = build(
            &engine,
            &setup.fixtures.storage_io,
            &limits,
            binding.clone(),
        )?;
        let read = instance
            .get_typed_func::<(), (u32,)>(&mut store, "read")
            .map_err(engine_error)?;
        let reply = read.call(&mut store, ()).map_err(engine_error)?;
        ensure(
            reply == (code,) && binding.calls.load(Ordering::SeqCst) == 1,
            "host-to-guest buffer boundary is wrong",
        )?;
    }
    Ok(())
}

/// Run the never-yielding fixture on `engine` under `deadline`; returns its outcome and the
/// time from arming to the end of the call.
fn spin(
    engine: &wasmtime::Engine,
    clock: &EpochClock,
    wat: &str,
    limits: &Limits,
    deadline: Duration,
) -> Result<(Outcome, Duration), Fail> {
    let (mut store, instance) = build(engine, wat, limits, Arc::new(Denying))?;
    let run = instance
        .get_typed_func::<(), ()>(&mut store, "run")
        .map_err(engine_error)?;
    let started = Instant::now();
    let guard = clock.arm(&mut store, deadline);
    let result = run.call(&mut store, ());
    let elapsed = started.elapsed();
    drop(guard);
    Ok((host::classify(store.data(), &result), elapsed))
}

pub fn guest_deadline_shared_engine(setup: &Setup) -> Check {
    let _serial = serial();
    let deadline = Duration::from_millis(300);
    let other = Duration::from_millis(900);
    let limits = replace(&setup.limits, Field::GuestDeadlineMs, 300)?;
    let engine = host::epoch_engine().map_err(engine_error)?;
    let clock = EpochClock::start(&engine).map_err(|_| failed("epoch ticker thread"))?;
    let wat = &setup.fixtures.infinite_loop;
    let (target, neighbour) = std::thread::scope(|scope| {
        let neighbour = scope.spawn(|| spin(&engine, &clock, wat, &limits, other));
        let target = spin(&engine, &clock, wat, &limits, limits.guest_deadline());
        (target, neighbour.join())
    });
    let (outcome, elapsed) = target?;
    let (neighbour_outcome, neighbour_elapsed) =
        neighbour.map_err(|_| failed("the neighbouring store panicked"))??;
    let bound = deadline + host::EPOCH_TICK + limits.interruption_tolerance();
    ensure(
        outcome == Outcome::GuestInterrupted && elapsed >= deadline && elapsed <= bound,
        "the infinite loop was not interrupted within the guest deadline plus tolerance",
    )?;
    // The neighbour kept running on the same engine until its own, later deadline.
    ensure(
        neighbour_outcome == Outcome::GuestInterrupted
            && neighbour_elapsed >= other
            && neighbour_elapsed > elapsed,
        "the neighbouring store was not interrupted at its own deadline",
    )?;
    ensure(
        clock.pending() == 0,
        "a deadline registration outlived its call",
    )
}

/// A binding that stays inside the host call until the gate opens.
struct Blocked {
    gate: Arc<(Mutex<bool>, Condvar)>,
}
impl Blocked {
    fn new() -> (Arc<Self>, Arc<(Mutex<bool>, Condvar)>) {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        (Arc::new(Self { gate: gate.clone() }), gate)
    }
}
impl Bindings for Blocked {
    fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
        let (open, wake) = &*self.gate;
        let mut open = open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let give_up = Instant::now() + Duration::from_secs(60);
        while !*open && Instant::now() < give_up {
            open = wake
                .wait_timeout(open, Duration::from_millis(50))
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        Ok(Reply::Data(vec![1]))
    }
}

fn release(gate: &(Mutex<bool>, Condvar)) {
    *gate
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
    gate.1.notify_all();
}

const TIMEOUT: u32 = 3;
const CANCELLED: u32 = 4;

pub fn host_call_deadline(setup: &Setup) -> Check {
    let _serial = serial();
    let tolerance = setup.limits.interruption_tolerance();
    // Guest interruption is disabled: a default engine has no epoch instrumentation, and
    // nothing is armed. Only the host-call deadline can end the blocked call.
    let engine = wasmtime::Engine::default();
    let deadline = Duration::from_millis(150);
    let limits = replace(&setup.limits, Field::HostCallDeadlineMs, 150)?;
    let (binding, gate) = Blocked::new();
    let (mut store, instance) = build(&engine, &setup.fixtures.storage_io, &limits, binding)?;
    let read = instance
        .get_typed_func::<(), (u32,)>(&mut store, "read")
        .map_err(engine_error)?;
    let started = Instant::now();
    let result = read.call(&mut store, ());
    let elapsed = started.elapsed();
    release(&gate);
    ensure(
        matches!(result, Ok((TIMEOUT,))),
        "the blocked host call did not end with a timeout",
    )?;
    ensure(
        host::classify(store.data(), &result) == Outcome::Uncertain,
        "a blocked host call must report uncertain, never success",
    )?;
    ensure(
        elapsed >= deadline && elapsed <= deadline + tolerance,
        "the host-call deadline did not end the call within its tolerance",
    )?;

    // Cancellation ends the same call independently of any deadline.
    let (binding, gate) = Blocked::new();
    let host = Host::with_limits(granted(), binding, setup.limits);
    let cancel = host.cancellation();
    let (mut store, instance) =
        host::instantiate(&engine, &setup.fixtures.storage_io, host).map_err(engine_error)?;
    let read = instance
        .get_typed_func::<(), (u32,)>(&mut store, "read")
        .map_err(engine_error)?;
    let after = Duration::from_millis(100);
    let started = Instant::now();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(after);
        cancel.store(true, Ordering::Release);
    });
    let result = read.call(&mut store, ());
    let elapsed = started.elapsed();
    release(&gate);
    canceller.join().map_err(|_| failed("canceller panicked"))?;
    ensure(
        matches!(result, Ok((CANCELLED,)))
            && host::classify(store.data(), &result) == Outcome::Cancelled,
        "cancellation did not end the blocked host call as cancelled",
    )?;
    ensure(
        elapsed >= after && elapsed <= after + tolerance,
        "cancellation did not end the call within its tolerance",
    )
}

/// Qualification-only termination fallback: the execution runs in a supervised child
/// process that the parent can kill and reap. It does not select the process model of
/// ADR 0004; the production model replaces it behind `TerminationFallback`.
pub struct ProcessFallback {
    program: PathBuf,
    mode: &'static str,
}

impl ProcessFallback {
    pub fn new(program: impl Into<PathBuf>, mode: &'static str) -> Self {
        Self {
            program: program.into(),
            mode,
        }
    }
}

pub struct ProcessExecution {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl TerminationFallback for ProcessFallback {
    type Running = ProcessExecution;

    fn start(&self, fixture: &str) -> io::Result<ProcessExecution> {
        let mut child = Command::new(&self.program)
            .args([OsString::from("--limits-child"), self.mode.into()])
            .env_clear()
            .env(FIXTURE_ENV, fixture)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child.stdin.take();
        let mut stdout = io::BufReader::new(child.stdout.take().ok_or(io::ErrorKind::BrokenPipe)?);
        let (ready, announced) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let _ = ready.send(stdout.read_line(&mut line).map(|_| line));
        });
        let mut running = ProcessExecution { child, stdin };
        match announced.recv_timeout(Duration::from_secs(30)) {
            Ok(Ok(line)) if line == "ready\n" => Ok(running),
            _ => {
                let _ = running.terminate();
                Err(io::Error::other(
                    "termination child did not announce readiness",
                ))
            }
        }
    }
}

impl Execution for ProcessExecution {
    fn request_cancel(&mut self) -> io::Result<()> {
        // Closing the pipe is the cancellation request; a child may ignore it.
        self.stdin.take();
        Ok(())
    }

    fn has_ended(&mut self) -> io::Result<bool> {
        Ok(self.child.try_wait()?.is_some())
    }

    fn terminate(&mut self) -> io::Result<()> {
        match self.child.kill() {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => {}
            Err(error) => return Err(error),
        }
        self.child.wait().map(drop)
    }
}

impl Drop for ProcessExecution {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

/// Body of `--limits-child <mode>`. `ignore-cancellation` runs the fixture guest forever
/// on an engine with no interruption and never reads its pipe; `honor-cancellation` stops
/// when the pipe closes.
pub fn child_main(mode: &str) -> std::process::ExitCode {
    use std::io::Write;
    let Ok(wat) = std::env::var(FIXTURE_ENV) else {
        return std::process::ExitCode::FAILURE;
    };
    let announce = || {
        let mut out = io::stdout().lock();
        let _ = out.write_all(b"ready\n");
        let _ = out.flush();
    };
    match mode {
        "honor-cancellation" => {
            announce();
            let _ = io::stdin().read_to_end(&mut Vec::new());
            std::process::ExitCode::SUCCESS
        }
        "ignore-cancellation" => {
            let engine = wasmtime::Engine::default();
            let host = Host::new(Context::default(), Arc::new(Denying));
            let Ok((mut store, instance)) = host::instantiate(&engine, wat, host) else {
                return std::process::ExitCode::FAILURE;
            };
            let Ok(run) = instance.get_typed_func::<(), ()>(&mut store, "run") else {
                return std::process::ExitCode::FAILURE;
            };
            announce();
            let _ = run.call(&mut store, ());
            std::process::ExitCode::FAILURE
        }
        _ => std::process::ExitCode::FAILURE,
    }
}

pub fn termination_fallback(setup: &Setup) -> Check {
    let _serial = serial();
    let Some(program) = &setup.termination_child else {
        return Err(Fail::Unverified("no termination child executable".into()));
    };
    let budget = setup.limits.termination_budget();
    let engine = wasmtime::Engine::default();
    let run = |mode: &'static str| -> Result<Ended, Fail> {
        let mut execution = ProcessFallback::new(program, mode)
            .start(&setup.fixtures.infinite_loop)
            .map_err(|e| failed(format!("termination child: {e}")))?;
        let ended = std::thread::scope(|scope| {
            // A valid fixture keeps completing while the other execution is being ended.
            let concurrent = scope.spawn(|| control(setup, &engine));
            let ended = host::end_execution(&mut execution, budget);
            (ended, concurrent.join())
        });
        let (ended, concurrent) = ended;
        concurrent.map_err(|_| failed("the concurrent fixture panicked"))??;
        let ended = ended.map_err(|e| failed(format!("termination: {e}")))?;
        ensure(
            execution.has_ended().unwrap_or(false),
            "the execution was not reaped",
        )?;
        Ok(ended)
    };
    // Control: a child that honours cancellation stops by itself.
    let honoured = run("honor-cancellation")?;
    ensure(
        matches!(honoured, Ended::Stopped { .. }),
        "a cooperative execution should stop on cancellation",
    )?;
    let ended = run("ignore-cancellation")?;
    ensure(
        matches!(ended, Ended::Terminated { elapsed } if elapsed <= budget)
            && ended.outcome() == Some(Outcome::Terminated),
        "an execution that ignores cancellation was not terminated within the budget",
    )
}
