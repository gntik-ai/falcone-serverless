//! In-pod runner of the execution API qualification (RUN-01.t3).
//!
//! The qualification runs the local test suite of the execution API, unchanged, inside the
//! restricted fs-test pod: the same cases, over real loopback sockets, with the worker's server
//! compiled from the same sources as the production image. This runner starts that suite's
//! executable, which the qualification image ships beside it, and turns each case result into
//! one evidence record next to the saturation cases. It never reads a credential: the suite's
//! credential is a synthetic fixture.

use std::process::{Command, ExitCode, Stdio};

/// The case whose inputs live in the repository's `docs/` tree, which the image does not ship;
/// the failure code table is checked by the offline tests.
const SKIPPED: &str = "the_code_table_the_schema_the_spec_and_the_code_enum_agree_on_every_code";

pub fn main() -> ExitCode {
    let Ok(this) = std::env::current_exe() else {
        return fail("current_exe");
    };
    let beside = |name: &str| this.with_file_name(name);
    let output = Command::new(beside("execute_api_local"))
        .args(["--test-threads=1", "--skip", SKIPPED])
        .env("EXECUTE_API_COMPILER", beside("artifact-compile-child"))
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let Ok(output) = output else {
        return fail("suite_not_started");
    };
    let mut cases = 0;
    let mut failures = 0;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some(rest) = line.strip_prefix("test ") else {
            continue;
        };
        let Some((case, status)) = rest.split_once(" ... ") else {
            continue;
        };
        let passed = status == "ok";
        cases += 1;
        failures += usize::from(!passed);
        println!(
            "{}",
            serde_json::json!({
                "check": "execute_api_case",
                "case": case,
                "status": if passed { "passed" } else { "failed" }
            })
        );
    }
    if output.status.success() && failures == 0 && cases > 0 {
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
