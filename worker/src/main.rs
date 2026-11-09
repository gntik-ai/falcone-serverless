//! Falcone serverless execution worker.
//!
//! Configuration checks and qualification-only component execution. No runtime adapter.

#[cfg_attr(not(test), allow(dead_code))]
mod checkpoint;

mod config;

#[cfg(test)]
mod runtime_metrics;

#[cfg(test)]
mod oci_execution_profile;

/// The worker's build identity, reported by `--version`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Run CLI arguments excluding the executable name; validation runs no guest code.
pub fn run(args: &[std::ffi::OsString]) -> i32 {
    run_with_io(
        args,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    )
}

/// CLI I/O boundary, also usable in-process without redirecting global streams.
pub fn run_with_io(
    args: &[std::ffi::OsString],
    stdout: &mut dyn std::io::Write,
    stderr: &mut dyn std::io::Write,
) -> i32 {
    match command(args, stdout) {
        Ok(()) => 0,
        Err(error) => {
            let _ = writeln!(stderr, "{error}");
            1
        }
    }
}

fn command(args: &[std::ffi::OsString], stdout: &mut dyn std::io::Write) -> Result<(), String> {
    let output = match args {
        [verb, artifact, params] if verb == "execute" => {
            let result = execute_command(artifact, params)?;
            return stdout.write_all(&result).map_err(|_| "write result".into());
        }
        [] => format!("falcone-serverless-worker {}", version()),
        [flag] if flag == "--version" => format!("falcone-serverless-worker {}", version()),
        [flag, path] if flag == "--check-config" => {
            let path = std::path::Path::new(path);
            let input = std::fs::read_to_string(path)
                .map_err(|error| format!("read config {}: {error}", path.display()))?;
            let config = config::load_from_str(&input)?;
            serde_json::to_string_pretty(&config).map_err(|error| error.to_string())?
        }
        _ => {
            return Err(
                "usage: falcone-serverless-worker [--version | --check-config PATH | execute COMPONENT PARAMS]".into(),
            );
        }
    };
    writeln!(stdout, "{output}").map_err(|error| error.to_string())
}

fn execute_command(
    artifact: &std::ffi::OsStr,
    params: &std::ffi::OsStr,
) -> Result<Vec<u8>, String> {
    use falcone_serverless_worker::host::{self, Bindings, Context, HostError, Operation, Reply};
    use falcone_serverless_worker::{artifact::supervisor::CompileLimits, cache, limits};
    use std::io::Read;
    use std::os::unix::fs::DirBuilderExt;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
    const MAX_COMPONENT_BYTES: usize = 64 * 1024 * 1024;
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    struct NoBindings;
    impl Bindings for NoBindings {
        fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
            Err(HostError::Denied)
        }
    }
    fn bounded(path: &std::ffi::OsStr, limit: usize, code: &str) -> Result<Vec<u8>, String> {
        let mut data = Vec::new();
        std::fs::File::open(path)
            .map_err(|_| "read invocation file")?
            .take(limit as u64 + 1)
            .read_to_end(&mut data)
            .map_err(|_| "read invocation file")?;
        if data.len() > limit {
            return Err(code.into());
        }
        Ok(data)
    }
    let artifact = bounded(artifact, MAX_COMPONENT_BYTES, "compile_size_limit_exceeded")?;
    let params = bounded(params, host::BUFFER_BYTES, "buffer_limit_exceeded")?;
    // Resolve only the installed platform sibling, never a tenant path or PATH.
    let executable = std::env::current_exe().map_err(|_| "compile_failed")?;
    let child = executable
        .parent()
        .ok_or("compile_failed")?
        .join("artifact-compile-child");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "trusted_cache_unavailable")?
        .as_nanos();
    let directory =
        std::env::temp_dir().join(format!("execute-cache-{}-{nonce}", std::process::id()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .map_err(|_| "trusted_cache_unavailable")?;
    let scratch = Scratch(directory);
    let mut cache = cache::Cache::new(&scratch.0, child).map_err(|error| error.code)?;
    let contract = limits::Limits::compatibility();
    let budgets = CompileLimits {
        input_bytes: MAX_COMPONENT_BYTES,
        cpu_seconds: contract.value(limits::Field::CompilerCpuSeconds),
        memory_bytes: contract.value(limits::Field::CompilerMemoryBytes),
        wall_clock_deadline: Instant::now() + Duration::from_secs(300),
    };
    let host = host::Host::new(Context::default(), std::sync::Arc::new(NoBindings));
    let loaded = cache
        .compile(
            cache::CONTRACT_VERSION,
            &artifact,
            budgets,
            &host.cancellation(),
        )
        .map_err(|error| error.code)?;
    host::execute(loaded, &params, host).map_err(|error| error.code.into())
}

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    std::process::exit(run(&args));
}

#[cfg(test)]
mod tests {
    use super::version;

    #[test]
    fn version_is_the_crate_version() {
        assert_eq!(version(), "0.1.0");
    }
}
