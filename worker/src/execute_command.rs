//! The `execute` subcommand: run one handler invocation from raw component bytes.
//!
//! Tenant bytes take the same path as any other intake. The cache validates them (Wasm magic,
//! imports, the single handler export) before anything is spawned, and only the supervised
//! trusted compiler child produces native code, which the cache authenticates before loading.
//! Native and precompiled input is rejected by that validation and never loaded.

use falcone_serverless_worker::artifact::supervisor::CompileLimits;
use falcone_serverless_worker::cache::{CONTRACT_VERSION, Cache};
use falcone_serverless_worker::host::execute::{Executor, Limits, Unbound};
use falcone_serverless_worker::host::{BUFFER_BYTES, Context, Host};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

pub const USAGE: &str = "usage: falcone-serverless-worker [--version | --check-config PATH | execute [--deadline-ms N] COMPONENT PARAMS]";

const MAX_COMPONENT_BYTES: usize = 64 * 1024 * 1024;
// Guest deadline defaults and ceiling follow the runtime config's wall-clock settings.
const DEFAULT_DEADLINE_MS: u64 = 30_000;
const MAX_DEADLINE_MS: u64 = 300_000;
// Compile budgets are the qualification control budgets, not production profile defaults.
const COMPILE_CPU_SECONDS: u64 = 120;
const COMPILE_MEMORY_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const COMPILE_WALL_CLOCK: Duration = Duration::from_secs(300);

struct Arguments<'a> {
    deadline: Duration,
    component: &'a Path,
    params: &'a Path,
}

fn parse(args: &[OsString]) -> Result<Arguments<'_>, String> {
    let usage = || USAGE.to_owned();
    let (deadline_ms, paths) = match args {
        [flag, value, paths @ ..] if flag == "--deadline-ms" => {
            let milliseconds = value
                .to_str()
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| (1..=MAX_DEADLINE_MS).contains(value))
                .ok_or_else(usage)?;
            (milliseconds, paths)
        }
        paths => (DEFAULT_DEADLINE_MS, paths),
    };
    match paths {
        [component, params] => Ok(Arguments {
            deadline: Duration::from_millis(deadline_ms),
            component: Path::new(component),
            params: Path::new(params),
        }),
        _ => Err(usage()),
    }
}

/// Read at most `limit` bytes plus one, so an oversized file is never slurped but is still
/// seen to exceed the limit by whoever enforces it.
fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    let unreadable = |_| "execute failed: input_unreadable".to_owned();
    let file = std::fs::File::open(path).map_err(unreadable)?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(unreadable)?;
    Ok(bytes)
}

/// A private scratch directory for this process's cache, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Self, String> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let path =
            std::env::temp_dir().join(format!("falcone-execute-{}-{nanos:x}", std::process::id()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|_| "execute failed: scratch_unavailable".to_owned())?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `args` follow the `execute` word. Success writes exactly the result bytes to `stdout`;
/// every failure writes nothing there and returns one `execute failed: CODE` line.
pub fn run(args: &[OsString], stdout: &mut dyn Write) -> Result<(), String> {
    let arguments = parse(args)?;
    let component = read_bounded(arguments.component, MAX_COMPONENT_BYTES)?;
    let params = read_bounded(arguments.params, BUFFER_BYTES)?;
    let compiler = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join("artifact-compile-child")))
        .ok_or("execute failed: compiler_unavailable")?;
    let scratch = Scratch::new()?;
    let mut cache = Cache::new(&scratch.0, compiler)
        .map_err(|error| format!("execute failed: {}", error.code))?;
    let limits = CompileLimits {
        input_bytes: MAX_COMPONENT_BYTES,
        cpu_seconds: COMPILE_CPU_SECONDS,
        memory_bytes: COMPILE_MEMORY_BYTES,
        wall_clock_deadline: Instant::now() + COMPILE_WALL_CLOCK,
    };
    let loaded = cache
        .compile(
            CONTRACT_VERSION,
            &component,
            limits,
            &AtomicBool::new(false),
        )
        .map_err(|error| match error.import {
            // Bounded to 256 characters by the validator; escaped, since it is artifact text.
            Some(import) => format!("execute failed: {}: {import:?}", error.code),
            None => format!("execute failed: {}", error.code),
        })?;
    let executor =
        Executor::new(cache.engine()).map_err(|failure| format!("execute failed: {failure}"))?;
    let host = Host::new(Context::default(), Arc::new(Unbound));
    let limits = Limits {
        deadline: arguments.deadline,
        // The deadline is the CPU bound here; fuel stays available to profile policy.
        fuel: u64::MAX,
    };
    let output = executor
        .execute(&loaded.component, host, &params, limits)
        .map_err(|failure| format!("execute failed: {failure}"))?;
    stdout
        .write_all(&output)
        .and_then(|()| stdout.flush())
        .map_err(|_| "execute failed: output_unwritable".to_owned())
}
