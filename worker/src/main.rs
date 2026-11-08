//! Falcone serverless execution worker.
//!
//! Binary entry point. `execute` runs one guest handler invocation through the validator,
//! the supervised trusted compiler and the capability host; every other command runs no guest
//! code and this binary embeds no engine itself.

#[cfg_attr(not(test), allow(dead_code))]
mod checkpoint;

mod config;

#[cfg(target_os = "linux")]
mod execute_command;

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

#[cfg(target_os = "linux")]
const USAGE: &str = execute_command::USAGE;
#[cfg(not(target_os = "linux"))]
const USAGE: &str = "usage: falcone-serverless-worker [--version | --check-config PATH]";

fn command(args: &[std::ffi::OsString], stdout: &mut dyn std::io::Write) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    if let [word, rest @ ..] = args
        && word == "execute"
    {
        // The result is raw bytes, written unchanged and without a trailing newline.
        return execute_command::run(rest, stdout);
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
        _ => return Err(USAGE.into()),
    };
    writeln!(stdout, "{output}").map_err(|error| error.to_string())
}

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    std::process::exit(run(&args));
}

#[cfg(test)]
mod tests {
    use super::{run_with_io, version};

    fn run_args(args: &[&str]) -> (i32, Vec<u8>, String) {
        let args: Vec<_> = args.iter().map(std::ffi::OsString::from).collect();
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let code = run_with_io(&args, &mut stdout, &mut stderr);
        (code, stdout, String::from_utf8(stderr).unwrap())
    }

    #[test]
    fn execute_rejects_bad_arguments_and_native_input_without_output() {
        for args in [
            &["execute"][..],
            &["execute", "one"],
            &["execute", "a", "b", "c"],
        ] {
            let (code, stdout, stderr) = run_args(args);
            assert_eq!(code, 1, "{args:?}");
            assert!(stdout.is_empty(), "{args:?}");
            assert!(
                stderr.starts_with("usage: falcone-serverless-worker"),
                "{args:?}"
            );
        }
        // A precompiled/native artifact stops at validation, before any compiler is needed.
        let dir = std::env::temp_dir().join(format!("worker-main-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (native, params) = (dir.join("native.elf"), dir.join("params"));
        std::fs::write(&native, b"\x7fELF\x02\x01\x01\0").unwrap();
        std::fs::write(&params, b"{}").unwrap();
        let (code, stdout, stderr) = run_args(&[
            "execute",
            native.to_str().unwrap(),
            params.to_str().unwrap(),
        ]);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        assert_eq!(stderr, "execute failed: precompiled_rejected\n");
    }

    #[test]
    fn version_is_the_crate_version() {
        assert_eq!(version(), "0.1.0");
    }
}
