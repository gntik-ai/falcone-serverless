//! `execute COMPONENT PARAMS`: raw component in, result bytes out.
//!
//! The component is never trusted: it passes `validate` (inside `Cache::compile`), is
//! compiled by the separate trusted `artifact-compile-child` process under CPU, memory and
//! wall-clock budgets, and is loaded only from that child's authenticated output. This
//! process has no platform bindings, so every capability call is denied.
use crate::artifact::supervisor::CompileLimits;
use crate::cache::Cache;
use falcone_serverless_worker::host::{self, Bindings, Context, Host, HostError, Operation, Reply};
use falcone_serverless_worker::limits::{Field, Limits};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

/// Contract version of the compile/cache boundary (`cache::CONTRACT_VERSION`).
const CONTRACT_VERSION: u32 = 3;
/// Largest raw component accepted for compilation.
const COMPONENT_BYTES: usize = 64 * 1024 * 1024;
/// Wall-clock allowance for validation, trusted compilation and cache verification.
const COMPILE_WALL_CLOCK: Duration = Duration::from_secs(300);

/// This process holds no credentials and no platform bindings.
struct NoBindings;
impl Bindings for NoBindings {
    fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
        Err(HostError::Denied)
    }
}

/// A fresh, process-private cache directory, removed on drop. Never adopts an existing one.
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Result<Self, String> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let path =
            std::env::temp_dir().join(format!("falcone-execute-{}-{nanos}", std::process::id()));
        std::fs::create_dir(&path).map_err(|_| "execute failed: scratch directory".to_owned())?;
        Ok(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Read at most `limit + 1` bytes, so an oversized file is detected without loading it.
fn read_bounded(path: &Path, limit: usize, what: &str) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|_| format!("execute failed: read {what}"))?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| format!("execute failed: read {what}"))?;
    Ok(bytes)
}

pub fn run(component: &Path, params: &Path) -> Result<Vec<u8>, String> {
    let limits = Limits::compatibility();
    let ceiling = limits.value(Field::HostToGuestBytes) as usize;
    let component = read_bounded(component, COMPONENT_BYTES, "component")?;
    let params = read_bounded(params, ceiling, "params")?;
    if params.len() > ceiling {
        return Err("execute failed: buffer_limit_exceeded".into());
    }
    let child = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join("artifact-compile-child")))
        .ok_or("execute failed: compiler unavailable")?;
    let scratch = Scratch::new()?;
    let mut cache =
        Cache::new(&scratch.0, child).map_err(|error| format!("execute failed: {}", error.code))?;
    let budget = CompileLimits {
        input_bytes: COMPONENT_BYTES,
        cpu_seconds: limits.value(Field::CompilerCpuSeconds),
        memory_bytes: limits.value(Field::CompilerMemoryBytes),
        wall_clock_deadline: Instant::now() + COMPILE_WALL_CLOCK,
    };
    let loaded = cache
        .compile(
            CONTRACT_VERSION,
            &component,
            budget,
            &AtomicBool::new(false),
        )
        .map_err(|error| match (error.import, error.export) {
            (Some(name), _) | (None, Some(name)) => {
                format!("execute failed: {}: {name}", error.code)
            }
            _ => format!("execute failed: {}", error.code),
        })?;
    let host = Host::with_limits(Context::default(), Arc::new(NoBindings), limits);
    host::execute::execute(&loaded.component, host, &params).map_err(|error| match error.message {
        Some(message) => format!("execute failed: {}: {message}", error.code),
        None => format!("execute failed: {}", error.code),
    })
}
