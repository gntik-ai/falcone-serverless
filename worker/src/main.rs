//! Falcone serverless execution worker.
//!
//! `execute COMPONENT PARAMS` is the one guest-running command: it validates the raw
//! component, compiles it in the trusted child process through the authenticated cache,
//! and calls the exported `handle` under the capability host's limits. Everything else
//! here (`--version`, `--check-config`) runs no guest code.

#[path = "artifact/mod.rs"]
pub mod artifact;
#[path = "cache/mod.rs"]
pub mod cache;

#[cfg_attr(not(test), allow(dead_code))]
mod checkpoint;

mod config;

mod execute;

#[cfg(test)]
mod runtime_metrics;

#[cfg(test)]
mod oci_execution_profile;

/// The worker's build identity, reported by `--version`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Run CLI arguments excluding the executable name.
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
    if let [verb, rest @ ..] = args
        && verb == "execute"
    {
        // Raw result bytes, no trailing newline; nothing is written on any failure.
        return match rest {
            [component, params] => {
                let result = execute::run(component.as_ref(), params.as_ref())?;
                stdout.write_all(&result).map_err(|error| error.to_string())
            }
            _ => Err("usage: falcone-serverless-worker execute COMPONENT PARAMS".into()),
        };
    }
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
        _ => {
            return Err(
                "usage: falcone-serverless-worker [--version | --check-config PATH | execute COMPONENT PARAMS]".into(),
            );
        }
    };
    writeln!(stdout, "{output}").map_err(|error| error.to_string())
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
