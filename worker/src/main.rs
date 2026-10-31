//! Falcone serverless execution worker.
//!
//! Placeholder binary so the workspace, CI and Hermes test commands exist before the
//! Wasmtime host lands (EP-RUN, #18). It embeds no engine and runs no guest code yet.

#[cfg_attr(not(test), allow(dead_code))]
mod checkpoint;

mod config;

#[cfg(test)]
mod runtime_metrics;

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
        _ => {
            return Err(
                "usage: falcone-serverless-worker [--version | --check-config PATH]".into(),
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
