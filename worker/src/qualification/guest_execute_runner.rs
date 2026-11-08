//! `compile-cache-qualification --guest-execute`: the cluster qualification of the guest
//! execute path. It runs every in-process case of `guest_execute_cases` inside the
//! restricted pod, plus the trusted-compile path the worker's `execute` command takes
//! (validate, supervised compile in the separate child, authenticated cache, execute).
//! One JSON record per case, then one aggregate record that appears only if all passed.
use crate::artifact::supervisor::CompileLimits;
use crate::cache::Cache;
use crate::guest_execute_cases::CASES;
use crate::handler_fixtures as fixtures;
use crate::host::execute::execute;
use crate::host::{Bindings, Context, Host, HostError, Operation, Reply};
use crate::limits::Limits;
use std::process::ExitCode;
use std::sync::{Arc, atomic::AtomicBool};
use std::time::{Duration, Instant};

struct Counting(std::sync::atomic::AtomicUsize);
impl Bindings for Counting {
    fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Ok(Reply::Empty)
    }
}

struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn budget() -> CompileLimits {
    CompileLimits {
        input_bytes: 64 * 1024 * 1024,
        cpu_seconds: 120,
        memory_bytes: 4 * 1024 * 1024 * 1024,
        wall_clock_deadline: Instant::now() + Duration::from_secs(300),
    }
}

/// The production entry path, with the real supervised compiler child.
fn trusted_compile_path() -> Result<(), String> {
    let path = std::env::temp_dir().join(format!("was03-guest-execute-{}", std::process::id()));
    std::fs::create_dir(&path).map_err(|_| "scratch create")?;
    let scratch = Scratch(path);
    let child = std::env::current_exe()
        .map_err(|_| "executable")?
        .parent()
        .ok_or("executable directory")?
        .join("artifact-compile-child");
    let mut cache = Cache::new(&scratch.0, child).map_err(|_| "cache create")?;
    let cancelled = AtomicBool::new(false);
    // A rejected component stops at intake: the compiler child is never even spawned.
    for (wasm, code) in [
        (
            fixtures::START_LOGS_WITHOUT_HANDLE,
            "invalid_handler_signature",
        ),
        (fixtures::WRONG_RESULT, "invalid_handler_signature"),
        (b"\0asm\x0d\0\x01\0".as_slice(), "missing_handler_export"),
    ] {
        match cache.compile(3, wasm, budget(), &cancelled) {
            Err(error) if error.code == code => {}
            _ => return Err("gate did not reject at intake".into()),
        }
    }
    if cache.spawn_count() != 0 {
        return Err("compiler spawned for a rejected component".into());
    }
    let loaded = cache
        .compile(3, fixtures::ECHO, budget(), &cancelled)
        .map_err(|_| "echo did not compile")?;
    let calls = Arc::new(Counting(Default::default()));
    let host = Host::with_limits(Context::default(), calls.clone(), Limits::compatibility());
    let result = execute(&loaded.component, host, b"{\"cluster\":true}")
        .map_err(|_| "echo did not execute")?;
    if result != b"{\"cluster\":true}" || cache.spawn_count() != 1 {
        return Err("echo through the trusted path was not exact".into());
    }
    Ok(())
}

fn emit(case: &str, result: Result<(), String>) -> bool {
    let passed = result.is_ok();
    println!(
        "{}",
        serde_json::json!({"check":"guest_execute","case":case,"status":if passed {"passed"} else {"failed"}})
    );
    if let Err(reason) = result {
        eprintln!("{case}: {reason}");
    }
    passed
}

pub fn main() -> ExitCode {
    let mut passed = true;
    for (name, case) in CASES {
        let result = std::panic::catch_unwind(case).map_err(|_| "case assertion failed".to_owned());
        passed &= emit(name, result);
    }
    passed &= emit("trusted_compile_path", trusted_compile_path());
    let summary = if passed {
        Ok(())
    } else {
        Err("one or more cases failed".to_owned())
    };
    let ok = summary.is_ok();
    println!(
        "{}",
        serde_json::json!({"check":"guest_execute","status":if ok {"passed"} else {"failed"}})
    );
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
