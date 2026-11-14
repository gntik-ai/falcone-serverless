//! Restricted-pod runner of the execution API cases. Built only with the `qualification`
//! feature.
//!
//! - `--execute-api[=CASE]`: runs the cases in process. Each starts the worker's server on
//!   loopback, in the pod's restricted posture, with a test platform.
//! - `--prepare DIR`: init-container mode; writes the production worker's configuration and
//!   artifacts under DIR.
//! - `--production-target`: runs the cases that need only the network against the production
//!   worker container of the same pod.
//!
//! The pod's verdict is this process's exit status and its JSON lines.
pub use falcone_serverless_worker::{api, host, server};

mod execute_api_cases;
mod execute_api_production;
mod execute_api_support;

use std::process::ExitCode;

fn record(name: &str, passed: bool) {
    let status = if passed { "passed" } else { "failed" };
    println!("{}", serde_json::json!({"case": name, "status": status}));
}

fn in_process(selected: Option<&str>) -> ExitCode {
    if selected.is_some_and(|only| {
        !execute_api_cases::CASES
            .iter()
            .any(|(name, _)| *name == only)
    }) {
        eprintln!("unknown_execute_api_case");
        return ExitCode::FAILURE;
    }
    let mut failed = false;
    for (name, case) in execute_api_cases::CASES {
        if selected.is_some_and(|only| only != *name) {
            continue;
        }
        let passed = std::panic::catch_unwind(case).is_ok();
        failed |= !passed;
        record(name, passed);
    }
    if failed {
        return ExitCode::FAILURE;
    }
    println!(
        "{}",
        serde_json::json!({"check": "execute_api", "status": "passed"})
    );
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [flag] if flag == "--execute-api" => in_process(None),
        [flag] if flag.starts_with("--execute-api=") => {
            in_process(flag.strip_prefix("--execute-api="))
        }
        [flag, directory] if flag == "--prepare" => {
            match execute_api_production::prepare(std::path::Path::new(directory)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(_) => ExitCode::FAILURE,
            }
        }
        [flag] if flag == "--production-target" => {
            if execute_api_production::run_all() {
                println!(
                    "{}",
                    serde_json::json!({"check": "execute_api_production", "status": "passed"})
                );
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        _ => {
            eprintln!(
                "usage: execute-api-qualification --execute-api[=CASE] | --prepare DIR | --production-target"
            );
            ExitCode::FAILURE
        }
    }
}
