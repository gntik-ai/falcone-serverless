//! Qualification-only re-exec supervisor. This does not select ADR 0004.
use crate::host::{self, Bindings, Context, Grants, Host, HostError, Operation, Reply};
use crate::limits::{CompileLimitError, Field, Limits};
use std::io::{Read, Write};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use wasmtime as engine;

const TOLERANCE: Duration = Duration::from_secs(2);

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Ready(Mutex<Option<Arc<AtomicBool>>>);
impl Bindings for Ready {
    fn call(&self, _: &Context, operation: Operation) -> Result<Reply, HostError> {
        if !matches!(operation, Operation::Publish { .. }) {
            return Err(HostError::Denied);
        }
        self.0
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .store(true, Ordering::Release);
        println!("cancellation-ignoring-guest-entered");
        std::io::stdout().flush().unwrap();
        Ok(Reply::Empty)
    }
}

pub fn unresponsive_child() -> ExitCode {
    let binding = Arc::new(Ready(Mutex::new(None)));
    let context = Context {
        grants: Grants {
            topics: vec!["orders".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let host = Host::new(context, binding.clone());
    *binding.0.lock().unwrap() = Some(host.cancellation());
    let engine = engine::Engine::default(); // No fuel or epoch: the guest ignores cancellation.
    let (mut store, instance) = host::instantiate(
        &engine,
        include_str!("limits_fixtures/delayed-mutation.wat"),
        host,
    )
    .unwrap();
    let run = instance
        .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
        .unwrap();
    let _ = run.call(&mut store, (4,));
    ExitCode::FAILURE // Returning is a qualification failure.
}

struct Empty;
impl Bindings for Empty {
    fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
        Err(HostError::Denied)
    }
}

fn valid_guest() -> Result<(), String> {
    let engine = engine::Engine::default();
    let (mut store, instance) = host::instantiate(
        &engine,
        include_str!("limits_fixtures/guest-limits.wat"),
        Host::new(Context::default(), Arc::new(Empty)),
    )
    .map_err(|_| "valid instantiate")?;
    let run = instance
        .get_typed_func::<(u32,), (u32,)>(&mut store, "run")
        .map_err(|_| "valid export")?;
    if run.call(&mut store, (0,)).map_err(|_| "valid call")? != (42,) {
        return Err("valid result".into());
    }
    Ok(())
}

pub fn termination(contract: Limits) -> Result<(), String> {
    let mut child = ChildGuard(
        Command::new(std::env::current_exe().map_err(|_| "executable")?)
            .arg("--limits-ignore-cancellation")
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "spawn")?,
    );
    let mut pipe = child.0.stdout.take().ok_or("stdout")?;
    rustix::fs::fcntl_setfl(&pipe, rustix::fs::OFlags::NONBLOCK).map_err(|_| "nonblocking pipe")?;
    let ready_deadline = Instant::now() + Duration::from_secs(10);
    let mut output = Vec::new();
    loop {
        let mut bytes = [0; 64];
        match pipe.read(&mut bytes) {
            Ok(0) => return Err("child exited before entering guest".into()),
            Ok(count) => output.extend_from_slice(&bytes[..count]),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return Err("child pipe".into()),
        }
        if output == b"cancellation-ignoring-guest-entered\n" {
            break;
        }
        if output.len() > 64 || Instant::now() >= ready_deadline {
            return Err("child readiness".into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let budget = contract.duration(Field::TerminationBudgetMs);
    let start = Instant::now();
    let valid = std::thread::spawn(valid_guest);
    // Readiness acknowledges the cooperative cancellation flag was set. Give
    // that cancellation exactly the validated grace budget, then escalate. This
    // supervisor owns only the re-exec qualification child, not production work.
    let deadline = start + budget;
    loop {
        if child.0.try_wait().map_err(|_| "child status")?.is_some() {
            return Err("guest did not ignore cancellation".into());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        std::thread::sleep(remaining.min(Duration::from_millis(2)));
    }
    child.0.kill().map_err(|_| "termination")?;
    let status = child.0.wait().map_err(|_| "reap")?;
    if status.success() || start.elapsed() > budget + TOLERANCE {
        return Err("termination budget".into());
    }
    if child.0.try_wait().map_err(|_| "reaped status")?.is_none() {
        return Err("child not reaped".into());
    }
    println!(
        "{}",
        serde_json::json!({
            "check": "termination_escalation",
            "budget_ms": contract.value(Field::TerminationBudgetMs),
            "elapsed_ms": start.elapsed().as_millis()
        })
    );
    valid.join().map_err(|_| "valid guest panic")??;
    Ok(())
}

fn emit(check: &str, result: Result<(), String>) -> bool {
    let passed = result.is_ok();
    println!(
        "{}",
        serde_json::json!({"check":check,"status":if passed {"passed"} else {"failed"}})
    );
    if let Err(reason) = result {
        eprintln!("{check}: {reason}");
    }
    passed
}

pub fn termination_main() -> ExitCode {
    let contract = std::env::args().nth(2).map_or_else(
        || Ok(Limits::compatibility()),
        |config| Limits::parse(&config).map_err(|_| "invalid termination limits".to_owned()),
    );
    if emit("termination_fallback", contract.and_then(termination)) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

pub fn main() -> ExitCode {
    let cases: &[(&str, fn())] = &[
        ("http_completion_cancellation", super::limits_cases::http_completion_at_cancellation_preserves_uncertainty),
        ("binding_timeout", super::limits_cases::binding_timeout_is_cancelled_only_for_reads),
        ("mutation_cancellation", super::limits_cases::cancellation_after_mutation_dispatch_never_claims_plain_cancelled),
        ("buffer_boundaries", super::limits_cases::configured_buffer_boundaries_are_enforced_before_dispatch_or_guest_lowering),
        ("guest_memory", super::limits_cases::guest_memory_exhaustion_is_typed_and_a_following_fixture_succeeds),
        ("guest_deadline", super::limits_cases::never_yielding_guest_obeys_the_configured_deadline),
        ("uncertain_interrupt", super::limits_cases::uncertain_is_not_hidden_by_a_later_epoch_interrupt),
        ("uncertain_memory", super::limits_cases::uncertain_is_not_hidden_by_a_later_memory_limit),
        ("blocked_read", super::limits_cases::blocked_read_uses_only_the_host_deadline_with_fuel_and_epochs_disabled),
    ];
    let mut passed = emit(
        "storage_key_buffer",
        std::panic::catch_unwind(
            super::limits_cases::configured_storage_keys_are_bounded_before_host_copy_or_binding,
        )
        .map_err(|_| "fixture assertion failed".to_owned()),
    );
    for (name, case) in cases {
        let result =
            std::panic::catch_unwind(*case).map_err(|_| "fixture assertion failed".to_owned());
        passed &= emit(name, result);
    }
    passed &= emit("compiler_cpu", compiler_case(CompileLimitError::Cpu));
    passed &= emit("compiler_memory", compiler_case(CompileLimitError::Memory));
    passed &= emit("termination_fallback", termination(Limits::compatibility()));
    if emit(
        "limits_cancellation",
        if passed {
            Ok(())
        } else {
            Err("one or more fixtures failed".into())
        },
    ) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn compiler_case(expected: CompileLimitError) -> Result<(), String> {
    use crate::{cache::Cache, compile_cache_fixtures as fixtures};
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "was02-limits-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&path).map_err(|_| "scratch create")?;
    let scratch = Scratch(path);
    let exe = std::env::current_exe().map_err(|_| "executable")?;
    let child = exe
        .parent()
        .ok_or("executable directory")?
        .join("artifact-compile-child");
    let mut cache = Cache::new(&scratch.0, child).map_err(|_| "cache create")?;
    let (cpu, memory, fixture) = match expected {
        CompileLimitError::Cpu => (1, 1024 * 1024 * 1024, fixtures::cpu()),
        CompileLimitError::Memory => (60, 128 * 1024 * 1024, fixtures::memory()),
        CompileLimitError::Other => return Err("invalid compiler case".into()),
    };
    let contract = Limits::parse(&serde_json::json!({
        "version":1,"compiler_cpu_seconds":cpu,"compiler_memory_bytes":memory,
        "guest_memory_bytes":268435456,"guest_to_host_bytes":1048576,"host_to_guest_bytes":1048576,
        "guest_deadline_ms":100,"host_call_deadline_ms":50,"termination_budget_ms":500
    }).to_string()).map_err(|_| "invalid compiler contract")?;
    let mut budget = fixtures::limits();
    budget.cpu_seconds = contract.value(Field::CompilerCpuSeconds);
    budget.memory_bytes = contract.value(Field::CompilerMemoryBytes);
    let cancelled = AtomicBool::new(false);
    let result = cache
        .compile(3, &fixture, budget, &cancelled)
        .map_err(|error| CompileLimitError::from_code(error.code));
    if result.err() != Some(expected) {
        return Err("wrong typed compiler limit".into());
    }
    if std::fs::read_dir(&scratch.0)
        .map_err(|_| "cache inspect")?
        .count()
        != 0
    {
        return Err("failed compilation published artifact/cache entry".into());
    }
    if cache
        .last_child_status()
        .map_err(|_| "compiler status")?
        .is_none()
    {
        return Err("compiler not reaped".into());
    }
    // The same owner must recover; success returns the only loadable artifact.
    let control = cache
        .compile(3, &fixtures::small(), budget, &cancelled)
        .map_err(|_| "compiler recovery")?;
    if control.cache_hit || cache.spawn_count() != 2 {
        return Err("recovery did not compile".into());
    }
    Ok(())
}
