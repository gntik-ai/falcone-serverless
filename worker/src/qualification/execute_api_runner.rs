//! In-pod runner of the execution API qualification (RUN-01.t3).
//!
//! The restricted fs-test pod runs the digest-pinned production worker image next to this
//! runner: an init step (`--execute-api-prepare`) writes the production worker's configuration
//! into a shared pod-local directory, the production image's own `falcone-serverless-worker
//! serve` runs as a sidecar, and this runner drives it over the pod's loopback. Two suites run,
//! one evidence record per case next to the saturation cases:
//!
//! - `execute_api_production`: the black-box suite against the production worker. It builds
//!   nothing from the worker's sources and needs no path from the build; the worker it talks to
//!   is the image under qualification. Identity, refusal, imports, credential absence,
//!   saturation and cancel, timeout selection, budgets and HTTP ceilings.
//! - `execute_api_local`: the hook-enabled suite (stalled setup, validation and cache stages,
//!   blocked host calls, connection and slow-reader deadlines). Its hooks exist only in this
//!   test build; the production image never contains them. It starts its own servers with the
//!   trusted compile child that this image ships.
//!
//! The runner never reads a credential: the suites' credential is a synthetic fixture.

use std::process::{Command, ExitCode, Stdio};

/// The pod-local directory shared with the production worker container.
const WORK_DIR: &str = "/work";
const EXECUTE_PORT: u16 = 8080;
const CONTROL_PORT: u16 = 8082;
/// What the worker is configured with, stated to the suite so that it never guesses.
const CONCURRENCY: u32 = 2;
const MAX_WALL_CLOCK_MS: u64 = 8000;
const MAX_MEMORY_BYTES: u64 = 64 * 1024 * 1024;

/// Writes the production worker's configuration and its artifact and cache directories.
pub fn prepare(directory: &str) -> ExitCode {
    let root = std::path::Path::new(directory);
    let config = format!(
        "version = 2\npool_id = \"pool-qualification\"\n\
         falcone_api_endpoint = \"https://api.invalid\"\n\
         falcone_dispatcher_endpoint = \"https://dispatcher.invalid\"\n\
         listen_port = {EXECUTE_PORT}\ncontrol_port = {CONTROL_PORT}\n\
         concurrency = {CONCURRENCY}\n\
         max_wall_clock_ms = {MAX_WALL_CLOCK_MS}\ndefault_wall_clock_ms = {MAX_WALL_CLOCK_MS}\n\
         max_memory_bytes = {MAX_MEMORY_BYTES}\ndefault_memory_bytes = {MAX_MEMORY_BYTES}\n\
         cache_path = \"{directory}/cache\"\nartifact_path = \"{directory}/artifacts\"\n"
    );
    let written = std::fs::create_dir_all(root.join("artifacts"))
        .and_then(|()| std::fs::create_dir_all(root.join("cache")))
        .and_then(|()| std::fs::write(root.join("worker.toml"), config));
    match written {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => fail("prepare_failed"),
    }
}

/// Totals of one suite's cases.
struct Suite {
    cases: usize,
    failures: usize,
    clean_exit: bool,
}

/// Runs one suite executable that sits beside this one and records each of its cases.
fn run_suite(name: &str, record: &str, env: &[(&str, String)]) -> Option<Suite> {
    let this = std::env::current_exe().ok()?;
    let mut command = Command::new(this.with_file_name(name));
    command
        .arg("--test-threads=1")
        .env(
            "EXECUTE_API_COMPILER",
            this.with_file_name("artifact-compile-child"),
        )
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command.output().ok()?;
    let mut suite = Suite {
        cases: 0,
        failures: 0,
        clean_exit: output.status.success(),
    };
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some((case, status)) = line
            .strip_prefix("test ")
            .and_then(|rest| rest.split_once(" ... "))
        else {
            continue;
        };
        let passed = status == "ok";
        suite.cases += 1;
        suite.failures += usize::from(!passed);
        println!(
            "{}",
            serde_json::json!({
                "check": record,
                "case": case,
                "status": if passed { "passed" } else { "failed" }
            })
        );
    }
    Some(suite)
}

pub fn main() -> ExitCode {
    // The image under qualification is named by the pod; no image, no qualification.
    let Ok(image) = std::env::var("EXECUTE_API_PRODUCTION_IMAGE") else {
        return fail("production_image_not_declared");
    };
    let production = run_suite(
        "execute_api_production",
        "execute_api_production_case",
        &[
            (
                "EXECUTE_API_TARGET",
                format!("{EXECUTE_PORT},{CONTROL_PORT}"),
            ),
            ("EXECUTE_API_ARTIFACTS", format!("{WORK_DIR}/artifacts")),
            (
                "EXECUTE_API_LIMITS",
                format!("{CONCURRENCY},{MAX_WALL_CLOCK_MS},{MAX_MEMORY_BYTES}"),
            ),
        ],
    );
    let hooks = run_suite("execute_api_local", "execute_api_case", &[]);
    let (Some(production), Some(hooks)) = (production, hooks) else {
        return fail("suite_not_started");
    };
    let passed = [&production, &hooks]
        .iter()
        .all(|suite| suite.clean_exit && suite.failures == 0 && suite.cases > 0);
    println!(
        "{}",
        serde_json::json!({
            "check": "execute_api_production",
            "image": image,
            "cases": production.cases,
            "status": if production.clean_exit && production.failures == 0 && production.cases > 0 {
                "passed"
            } else {
                "failed"
            }
        })
    );
    if passed {
        println!(
            "{}",
            serde_json::json!({"check": "execute_api", "status": "passed"})
        );
        ExitCode::SUCCESS
    } else {
        fail("cases_failed")
    }
}

fn fail(reason: &str) -> ExitCode {
    println!(
        "{}",
        serde_json::json!({"check": "execute_api", "status": "failed", "reason": reason})
    );
    ExitCode::FAILURE
}
