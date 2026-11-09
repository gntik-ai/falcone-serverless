//! Falcone serverless execution worker.
//!
//! Explicit local handler execution; no runtime adapter or platform routing.

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
        [] => format!("falcone-serverless-worker {}", version()),
        [flag] if flag == "--version" => format!("falcone-serverless-worker {}", version()),
        [flag, path] if flag == "--check-config" => {
            let path = std::path::Path::new(path);
            let input = std::fs::read_to_string(path)
                .map_err(|error| format!("read config {}: {error}", path.display()))?;
            let config = config::load_from_str(&input)?;
            serde_json::to_string_pretty(&config).map_err(|error| error.to_string())?
        }
        [command, component, params] if command == "execute" => {
            return execute_command(component, params, None, stdout);
        }
        [command, component, params, limits] if command == "execute" => {
            return execute_command(component, params, Some(limits), stdout);
        }
        _ => {
            return Err(
                "usage: falcone-serverless-worker [--version | --check-config PATH | execute COMPONENT PARAMS [LIMITS]]".into(),
            );
        }
    };
    writeln!(stdout, "{output}").map_err(|error| error.to_string())
}

fn execute_command(
    component: &std::ffi::OsStr,
    params: &std::ffi::OsStr,
    limits_path: Option<&std::ffi::OsStr>,
    stdout: &mut dyn std::io::Write,
) -> Result<(), String> {
    use falcone_serverless_worker::{
        host,
        limits::{Field, Limits},
    };
    use std::io::Read;
    struct Denied;
    impl host::Bindings for Denied {
        fn call(
            &self,
            _: &host::Context,
            _: host::Operation,
        ) -> Result<host::Reply, host::HostError> {
            Err(host::HostError::Denied)
        }
    }
    let limits = match limits_path {
        Some(path) => {
            let input = read_bounded(path, 16384)?;
            Limits::parse(std::str::from_utf8(&input).map_err(|_| "invalid_limits")?)
                .map_err(|_| "invalid_limits")?
        }
        None => Limits::compatibility(),
    };
    fn read_bounded(path: &std::ffi::OsStr, limit: usize) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|_| "input_unavailable")?
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "input_unavailable")?;
        if bytes.len() > limit {
            return Err("buffer_limit_exceeded".into());
        }
        Ok(bytes)
    }
    let params = read_bounded(params, limits.value(Field::HostToGuestBytes) as usize)?;
    let bytes = read_bounded(component, 64 * 1024 * 1024)?;
    let mut config = wasmtime::Config::new();
    config.epoch_interruption(true);
    let engine = wasmtime::Engine::new(&config).map_err(|_| "engine_unavailable")?;
    let host = host::Host::with_limits(
        host::Context::default(),
        std::sync::Arc::new(Denied),
        limits,
    );
    let result = host::execute(&engine, &bytes, host, &params)
        .result
        .map_err(|error| error.code.to_owned())?;
    stdout
        .write_all(&result)
        .map_err(|_| "output_unavailable".into())
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
