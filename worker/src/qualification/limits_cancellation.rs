//! WAS-02 limits evidence. This process fallback qualifies trusted fixtures only;
//! it neither selects ADR 0004's production model nor makes side effects replay-safe.
use super::{
    artifact::supervisor::CompileLimits, capability_invocations as fixtures, limits_evidence,
};
use falcone_serverless_worker::{
    host::ExecutionOutcome,
    limits::{Field, Limits},
};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn contract() -> Limits {
    Limits::parse(include_str!("fixtures/limits-v1.json")).expect("embedded qualification contract")
}

// artifact types remain mounted only by binaries, outside the limits contract.
fn compile_limits(limits: &Limits) -> CompileLimits {
    CompileLimits {
        cpu_seconds: limits.get(Field::CompileCpuSeconds),
        memory_bytes: limits.get(Field::CompileMemoryBytes),
        input_bytes: limits.get(Field::CompileInputBytes) as usize,
        wall_clock_deadline: Instant::now()
            + Duration::from_millis(limits.get(Field::CompileWallClockMs)),
    }
}

fn compile_case(cpu: bool) -> Result<Value, String> {
    let name = if cpu { "cpu" } else { "memory" };
    let root = std::env::temp_dir().join(format!("was02-limits-{}-{name}", std::process::id()));
    std::fs::create_dir(&root).map_err(|_| "scratch_create")?;
    let result = (|| {
        let exe = std::env::current_exe().map_err(|_| "executable_path")?;
        let child = exe
            .parent()
            .ok_or("executable_directory")?
            .join("artifact-compile-child");
        let source = include_str!("fixtures/limits-v1.json");
        let failure = if cpu {
            source
                .replace("\"compile_cpu_seconds\":120", "\"compile_cpu_seconds\":1")
                .replace("4294967296", "1073741824")
        } else {
            source
                .replace("\"compile_cpu_seconds\":120", "\"compile_cpu_seconds\":60")
                .replace("4294967296", "134217728")
        };
        let limits = Limits::parse(&failure).map_err(|error| error.to_string())?;
        let bytes = if cpu {
            super::compile_cache_fixtures::cpu()
        } else {
            super::compile_cache_fixtures::memory()
        };
        let diagnostic = if cpu {
            "compile_cpu_limit_exceeded"
        } else {
            "compile_memory_limit_exceeded"
        };
        super::budget_case(
            &root.join("cache"),
            &child,
            &bytes,
            compile_limits(&limits),
            compile_limits(&contract()),
            diagnostic,
        )?;
        Ok(
            json!({"outcome":diagnostic,"reaped":true,"published_on_failure":false,"recovery_compiled":true}),
        )
    })();
    std::fs::remove_dir_all(root).map_err(|_| "scratch_cleanup")?;
    result
}

struct ExecutionChild(Child);
impl Drop for ExecutionChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn marker(child: &mut Child, expected: &str, deadline: Instant) -> Result<(), String> {
    let stdout = child.stdout.as_mut().ok_or("child_stdout")?;
    let mut bytes = Vec::new();
    while Instant::now() < deadline {
        let mut byte = [0];
        match stdout.read(&mut byte) {
            Ok(0) => return Err("child exited before marker".into()),
            Ok(_) => {
                bytes.push(byte[0]);
                if byte[0] == b'\n' {
                    return if bytes == format!("{expected}\n").as_bytes() {
                        Ok(())
                    } else {
                        Err("wrong child marker".into())
                    };
                }
                if bytes.len() > 128 {
                    return Err("oversized child marker".into());
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                std::thread::sleep(Duration::from_millis(1))
            }
            Err(_) => return Err("child marker read".into()),
        }
    }
    Err("child marker deadline".into())
}

fn termination() -> Result<Value, String> {
    let exe = std::env::current_exe().map_err(|_| "executable_path")?;
    let mut child = ExecutionChild(
        Command::new(exe)
            .arg("--limits-stubborn-child")
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "child_spawn")?,
    );
    rustix::fs::fcntl_setfl(
        child.0.stdout.as_ref().ok_or("child_stdout")?,
        rustix::fs::OFlags::NONBLOCK,
    )
    .map_err(|_| "child_nonblocking")?;
    marker(
        &mut child.0,
        "limits-child-ready",
        Instant::now() + Duration::from_secs(10),
    )?;
    let cancelled_at = Instant::now();
    child
        .0
        .stdin
        .as_mut()
        .ok_or("child_stdin")?
        .write_all(b"cancel\n")
        .map_err(|_| "cancel_request")?;
    let budget = Duration::from_millis(contract().get(Field::TerminationBudgetMs));
    marker(&mut child.0, "cancel-received", cancelled_at + budget)?;
    while cancelled_at.elapsed() < budget {
        if child.0.try_wait().map_err(|_| "child_status")?.is_some() {
            return Err("child did not ignore cancellation".into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let mut signal = None;
    fixtures::valid_guest_overlapping_termination(|| {
        child.0.kill().map_err(|_| "child_kill")?;
        let status = child.0.wait().map_err(|_| "child_reap")?;
        signal = status.signal();
        if signal != Some(9) || child.0.try_wait().map_err(|_| "child_status")?.is_none() {
            return Err("child not SIGKILLed and reaped".into());
        }
        Ok(())
    })?;
    if cancelled_at.elapsed() > budget + Duration::from_secs(2) {
        return Err("termination budget exceeded".into());
    }
    Ok(
        json!({"outcome":ExecutionOutcome::Terminated,"signal":signal,"reaped":true,"overlapping_valid_guest":true,"cancel_elapsed_ms":cancelled_at.elapsed().as_millis()}),
    )
}

fn record(name: &str, expected: &str, execute: impl FnOnce() -> Result<Value, String>) -> bool {
    let started = Instant::now();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(execute))
        .unwrap_or_else(|_| Err("fixture assertion failed".into()));
    let evidence = result.as_ref().ok().cloned();
    let observed = evidence
        .as_ref()
        .and_then(|value| value["outcome"].as_str());
    let elapsed = if name == "termination" {
        evidence
            .as_ref()
            .and_then(|value| value["cancel_elapsed_ms"].as_u64())
            .map(Duration::from_millis)
            .unwrap_or(started.elapsed())
    } else {
        started.elapsed()
    };
    let maximum = match name {
        "guest_deadline" => Some(
            Duration::from_millis(contract().get(Field::GuestDeadlineMs)) + Duration::from_secs(2),
        ),
        "host_call_deadline" => Some(
            Duration::from_millis(contract().get(Field::HostCallDeadlineMs))
                + Duration::from_secs(2),
        ),
        "termination" => Some(
            Duration::from_millis(contract().get(Field::TerminationBudgetMs))
                + Duration::from_secs(2),
        ),
        _ => None,
    };
    let status = limits_evidence::limits_fixture(
        true,
        observed,
        expected,
        elapsed,
        maximum,
        result.map(|_| ()),
    );
    let mut value = serde_json::to_value(status).expect("outcome JSON");
    let passed = value["status"] == "passed";
    value["check"] = json!(name);
    value["evidence"] = json!(evidence);
    println!("{value}");
    passed
}

pub fn run() -> std::process::ExitCode {
    let mut passed = record("compile_cpu", "compile_cpu_limit_exceeded", || {
        compile_case(true)
    });
    passed &= record("compile_memory", "compile_memory_limit_exceeded", || {
        compile_case(false)
    });
    let cases: [(&str, fn(), &str); 6] = [
        (
            "guest_memory",
            fixtures::guest_memory_denial_records_a_host_event_even_when_the_guest_recovers,
            "guest-memory-limit",
        ),
        (
            "guest_to_host",
            || {
                fixtures::configured_guest_buffer_checks_the_entire_aggregate_before_binding();
                fixtures::configured_storage_keys_are_checked_before_copying_or_binding();
            },
            "buffer-limit",
        ),
        (
            "storage_reply",
            fixtures::configured_storage_reply_is_not_delivered_above_the_aggregate_limit,
            "buffer-limit",
        ),
        (
            "http_reply",
            fixtures::configured_http_reply_is_not_delivered_above_the_aggregate_limit,
            "buffer-limit",
        ),
        (
            "guest_deadline",
            fixtures::configured_guest_deadline_interrupts_an_infinite_loop,
            "guest-interrupted",
        ),
        (
            "host_call_deadline",
            fixtures::host_call_deadline_alone_records_uncertain_until_binding_exit_is_observed,
            "host-call-cancelled",
        ),
    ];
    for (name, execute, expected) in cases {
        passed &= record(name, expected, || {
            execute();
            Ok(json!({"outcome":expected}))
        });
    }
    passed &= record("termination", "terminated", termination);
    if passed {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}
