//! All import checks precede cache construction, compilation and guest instantiation.
use super::{
    Service,
    attempt::{Attempt, Registration},
    error,
};
use crate::{
    api::{ExecuteRequest, Pinned},
    artifact::{cache_key::ENGINE_VERSION, supervisor::CompileLimits, validate::validate},
    cache::{CONTRACT_VERSION, Cache},
    config::RuntimeConfig,
    host,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::{Arc, Mutex, atomic::Ordering};
use std::time::{Duration, Instant};

const MAX_COMPONENT: usize = 64 * 1024 * 1024;

pub(super) fn component(
    config: &RuntimeConfig,
    request: &ExecuteRequest,
) -> Result<Vec<u8>, (u16, Value)> {
    let projection = request.projection();
    // Only a validated digest selects a file in the trusted component spool.
    let hex = projection
        .component_digest()
        .strip_prefix("sha256:")
        .ok_or_else(|| error(422, "invalid_module"))?;
    let path = std::path::Path::new(&config.cache_path)
        .join("components")
        .join(format!("{hex}.wasm"));
    let file = OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(path)
        .map_err(|_| error(422, "invalid_module"))?;
    if !file
        .metadata()
        .map_err(|_| error(422, "invalid_module"))?
        .is_file()
    {
        return Err(error(422, "invalid_module"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_COMPONENT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| error(422, "invalid_module"))?;
    if format!("{:x}", Sha256::digest(&bytes)) != hex {
        return Err(error(422, "invalid_module"));
    }
    let validated = validate(&bytes, MAX_COMPONENT).map_err(|e| error(422, e.code))?;
    // validate() enforces ALLOWED_IMPORTS. This second, per-request check also runs on
    // cache hits, so a previous projection cannot confer authority on this one.
    projection
        .check_imports(&validated.imports)
        .map_err(|_| error(422, "undeclared_import"))?;
    Ok(bytes)
}

#[derive(Default)]
struct Logs(Mutex<Vec<String>>);
impl host::Bindings for Logs {
    fn call(
        &self,
        _: &host::Context,
        operation: host::Operation,
    ) -> Result<host::Reply, host::HostError> {
        // Platform capability calls stay denied until Falcone supplies the reviewed
        // dispatcher authentication/credential-verification boundary (INV-01).
        match operation {
            host::Operation::Log {
                message,
                fields,
                severity,
            } => {
                let mut logs = self
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if logs.len() < 1000 {
                    let severity = match severity {
                        host::Level::Debug => "debug",
                        host::Level::Info => "info",
                        host::Level::Warn => "warn",
                        host::Level::Error => "error",
                    };
                    let fields: Vec<_> = fields
                        .into_iter()
                        .map(|field| serde_json::json!({"name":field.name,"value":field.value}))
                        .collect();
                    let line =
                        serde_json::json!({"severity":severity,"message":message,"fields":fields})
                            .to_string();
                    // Do not retain a truncated credential fragment at the line boundary.
                    logs.push(if line.chars().count() <= 8192 {
                        line
                    } else {
                        "<redacted>".into()
                    });
                }
                Ok(host::Reply::Empty)
            }
            _ => Err(host::HostError::Denied),
        }
    }
}

pub(super) fn execute(
    service: &Service,
    request: ExecuteRequest,
) -> Result<(u16, Value), (u16, Value)> {
    let bytes = component(&service.config, &request)?;
    if request.projection().engine_version() != ENGINE_VERSION {
        return Err(error(422, "unsupported_feature"));
    }
    let (invocation_key, attempt_id, idempotency_key) = request.invocation_identity();
    let logs = Arc::new(Logs::default());
    let host = host::Host::new(
        host::Context {
            invocation_key,
            attempt_id,
            idempotency_key,
            ..Default::default()
        },
        logs.clone(),
    );
    // The complete request (including its credential) stays in this handler frame.
    // Neither store data, WASI, preopens, bindings nor log sinks hold that credential.
    let attempt = Attempt::new(host.cancellation());
    let registration = Registration::new(&service.attempts, request.key(), attempt.clone())
        .ok_or_else(|| error(503, "busy"))?;
    let loaded = {
        let mut cache = service
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if cache.is_none() {
            let compiler = std::env::current_exe()
                .map_err(|_| error(500, "compile_failed"))?
                .with_file_name("artifact-compile-child");
            *cache = Some(
                Cache::new(
                    std::path::Path::new(&service.config.cache_path).join("native"),
                    compiler,
                )
                .map_err(|e| error(500, e.code))?,
            );
        }
        cache
            .as_mut()
            .ok_or_else(|| error(500, "trusted_cache_unavailable"))?
            .compile(
                CONTRACT_VERSION,
                &bytes,
                CompileLimits {
                    input_bytes: MAX_COMPONENT,
                    cpu_seconds: 120,
                    memory_bytes: 4 * 1024 * 1024 * 1024,
                    wall_clock_deadline: Instant::now() + Duration::from_secs(300),
                },
                &attempt.cancelled,
            )
            .map_err(|e| error(422, e.code))?
    };
    let params = request
        .params_bytes()
        .map_err(|_| error(400, "invalid_request"))?;
    let execution = match host::Prepared::new(&loaded, host) {
        Ok(prepared) => {
            attempt.running.store(true, Ordering::Release);
            prepared.call(&params)
        }
        Err(execution) => execution,
    };
    registration.attempt.complete(execution.outcome);
    let result = execution.result.map_err(|e| error(422, e.code))?;
    let logs = logs
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let result = crate::api::build_result(&request, &result, logs)
        .map_err(|_| error(422, "invalid_input"))?;
    Ok((
        200,
        serde_json::to_value(result).map_err(|_| error(500, "invalid_input"))?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use host::outcome::InvocationOutcome;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn cancel_after_guest_completion_does_not_acknowledge_an_interruption() {
        for outcome in [
            InvocationOutcome::Success,
            InvocationOutcome::Failed,
            InvocationOutcome::GuestMemoryLimit,
            InvocationOutcome::BufferLimit,
            InvocationOutcome::Uncertain,
        ] {
            let attempt = Attempt::new(Arc::new(AtomicBool::new(false)));
            // The guest outcome is already final when cancel arrives, but the
            // execution handler has not yet published completion to its waiter.
            assert!(!attempt.cancel(Duration::ZERO));
            attempt.complete(outcome);
            assert!(
                !attempt.cancel(Duration::ZERO),
                "late cancel acknowledged for {outcome:?}"
            );
        }
    }

    #[test]
    fn cancellation_waiter_acknowledges_only_a_completed_interruption() {
        for outcome in [
            InvocationOutcome::GuestInterrupted,
            InvocationOutcome::Cancelled,
        ] {
            let attempt = Attempt::new(Arc::new(AtomicBool::new(false)));
            std::thread::scope(|scope| {
                let waiter = scope.spawn(|| attempt.cancel(Duration::from_secs(5)));
                let deadline = Instant::now() + Duration::from_secs(5);
                while !attempt.cancelled.load(Ordering::Acquire) {
                    assert!(Instant::now() < deadline, "cancel never requested");
                    std::thread::yield_now();
                }
                attempt.complete(outcome);
                assert!(waiter.join().unwrap(), "interruption not acknowledged");
            });
        }
    }

    #[test]
    fn completion_before_cancel_does_not_acknowledge_a_later_request() {
        for outcome in [
            InvocationOutcome::Success,
            InvocationOutcome::Failed,
            InvocationOutcome::GuestMemoryLimit,
            InvocationOutcome::BufferLimit,
            InvocationOutcome::Uncertain,
            InvocationOutcome::GuestInterrupted,
            InvocationOutcome::Cancelled,
        ] {
            let attempt = Attempt::new(Arc::new(AtomicBool::new(false)));
            attempt.complete(outcome);
            assert!(!attempt.cancel(Duration::ZERO));
        }
    }
}
