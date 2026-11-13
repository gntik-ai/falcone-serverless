//! Falcone serverless execution worker.
//!
//! Explicit local handler execution and the execution API service (`serve`); no platform routing.

// The service uses the execute, cancel and update halves of the API. The parsers only the
// dispatcher calls (`parse_projection`, `parse_result`) stay unused until it adopts the API; the
// dispatcher issue that first calls them removes this allowance.
#[cfg_attr(not(test), allow(dead_code))]
mod api;

#[cfg_attr(not(test), allow(dead_code))]
mod checkpoint;

mod config;

// The service runs on the lib's engine, cache, host and limits; `api` reaches the typed limits
// through this path.
use falcone_serverless_worker::limits;

mod server;

#[cfg(test)]
mod runtime_metrics;

#[cfg(test)]
mod oci_execution_profile;

/// The worker's build identity, reported by `--version`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Run CLI arguments excluding the executable name, including explicit execution.
pub fn run(args: &[std::ffi::OsString]) -> i32 {
    // The service writes its structured log from many threads: no thread may hold the standard
    // streams for the life of the process, as the one-shot commands below do.
    if let [command, path] = args
        && command == "serve"
    {
        return match serve_command(path) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("{error}");
                1
            }
        };
    }
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
        [command, _] if command == "serve" => {
            return Err("serve is run by `run`, not in process".into());
        }
        [command, component, params] if command == "execute" => {
            return execute_command(component, params, None, stdout);
        }
        [command, component, params, limits] if command == "execute" => {
            return execute_command(component, params, Some(limits), stdout);
        }
        _ => {
            return Err(
                "usage: falcone-serverless-worker [--version | --check-config PATH | serve CONFIG | execute COMPONENT PARAMS [LIMITS]]".into(),
            );
        }
    };
    writeln!(stdout, "{output}").map_err(|error| error.to_string())
}

/// Serves the execution API (RUN-01): the execute listener on `listen_port` and the control
/// listener on `control_port`, until the process ends.
fn serve_command(path: &std::ffi::OsStr) -> Result<(), String> {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    let path = std::path::Path::new(path);
    let input = std::fs::read_to_string(path)
        .map_err(|error| format!("read config {}: {error}", path.display()))?;
    let config = config::load_from_str(&input)?;
    // The trusted compiler is resolved beside this executable; no request can select it.
    let compiler = std::env::current_exe()
        .map_err(|_| "compile_failed".to_owned())?
        .with_file_name("artifact-compile-child");
    let cache_path = std::path::PathBuf::from(&config.cache_path);
    let any = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
    let server_config = server::ServerConfig {
        execute_addr: SocketAddr::new(any, config.listen_port),
        control_addr: SocketAddr::new(any, config.control_port),
        concurrency: config.concurrency as usize,
        ceilings: api::WorkerCeilings {
            max_memory_bytes: config.max_memory_bytes,
            max_wall_clock_ms: config.max_wall_clock_ms,
        },
        cancel_deadline: std::time::Duration::from_millis(config.cancel_reserve_ms),
        artifact_dir: cache_path.join("artifacts"),
        cache_dir: cache_path.join("compiled"),
        compiler,
        deadlines: server::Deadlines::default(),
    };
    // Platform bindings (storage, events, HTTP, workflows) arrive with the dispatcher; until
    // then every host capability except logging is denied.
    server::run(server_config, std::sync::Arc::new(NoPlatform)).map_err(|error| error.to_string())
}

/// No platform behind the host capabilities: everything but logging is denied.
struct NoPlatform;

impl falcone_serverless_worker::host::Bindings for NoPlatform {
    fn call(
        &self,
        _: &falcone_serverless_worker::host::Context,
        _: falcone_serverless_worker::host::Operation,
    ) -> Result<falcone_serverless_worker::host::Reply, falcone_serverless_worker::host::HostError>
    {
        Err(falcone_serverless_worker::host::HostError::Denied)
    }
}

fn execute_command(
    component: &std::ffi::OsStr,
    params: &std::ffi::OsStr,
    limits_path: Option<&std::ffi::OsStr>,
    stdout: &mut dyn std::io::Write,
) -> Result<(), String> {
    use falcone_serverless_worker::{
        artifact::supervisor::CompileLimits,
        cache::{CONTRACT_VERSION, Cache},
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
    // Resolve the platform compiler beside this trusted executable; no request
    // parameter or PATH lookup can select a tenant executable.
    let compiler = std::env::current_exe()
        .map_err(|_| "compile_failed")?
        .with_file_name("artifact-compile-child");
    let scratch = Scratch::new()?;
    let mut cache = Cache::new(&scratch.0, compiler).map_err(|error| error.code)?;
    // Explicit local-command budgets, separate from the guest invocation clock.
    // Deployment profile selection remains the runtime adapter's responsibility.
    let compile_limits = CompileLimits {
        input_bytes: 64 * 1024 * 1024,
        cpu_seconds: 120,
        memory_bytes: 4 * 1024 * 1024 * 1024,
        wall_clock_deadline: std::time::Instant::now() + std::time::Duration::from_secs(300),
    };
    let host = host::Host::with_limits(
        host::Context::default(),
        std::sync::Arc::new(Denied),
        limits,
    );
    let loaded = cache
        .compile(
            CONTRACT_VERSION,
            &bytes,
            compile_limits,
            &host.cancellation(),
        )
        .map_err(|error| error.code)?;
    let result = host::execute(&loaded, host, &params)
        .result
        .map_err(|error| error.code.to_owned())?;
    stdout
        .write_all(&result)
        .map_err(|_| "output_unavailable".into())
}

/// A private process-owned cache directory; no cache authority survives exit.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new() -> Result<Self, String> {
        use ring::rand::SecureRandom;
        use std::os::unix::fs::DirBuilderExt;
        let mut nonce = [0; 16];
        ring::rand::SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| "trusted_cache_unavailable")?;
        let path = std::env::temp_dir().join(format!(
            "falcone-execute-{:032x}",
            u128::from_be_bytes(nonce)
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|_| "trusted_cache_unavailable")?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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
