use super::{Active, Listener, Phase, State};
use crate::{api, artifact::supervisor::CompileLimits, cache::CONTRACT_VERSION, host};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::sync::{Arc, Condvar, Mutex, atomic::Ordering};
use std::time::{Duration, Instant};

/// No platform connector is invented here: external operations fail closed.
/// Only the host owns the credential; no WASI linker, environment or preopens exist.
struct HostBindings {
    _credential: api::Credential,
    logs: Arc<Mutex<Vec<String>>>,
}
impl host::Bindings for HostBindings {
    fn call(
        &self,
        _: &host::Context,
        operation: host::Operation,
    ) -> Result<host::Reply, host::HostError> {
        match operation {
            host::Operation::Log {
                severity,
                message,
                fields,
            } => {
                let mut logs = self.logs.lock().unwrap();
                if logs.len() < 1000 {
                    let severity = format!("{severity:?}");
                    let record = LogRecord {
                        severity: &severity,
                        message: &message,
                        fields: LogFields(&fields),
                    };
                    let mut output = LogLine {
                        text: String::new(),
                        left: 8192,
                        invalid: false,
                    };
                    // The writer stops serialization at the whole-line ceiling;
                    // never truncate individual values and then add delimiters.
                    let _ = serde_json::to_writer(&mut output, &record);
                    logs.push(if output.invalid {
                        "<redacted>".into()
                    } else {
                        output.text
                    });
                }
                Ok(host::Reply::Empty)
            }
            host::Operation::Progress { .. } => Ok(host::Reply::Empty),
            _ => Err(host::HostError::Denied),
        }
    }
}

#[derive(serde::Serialize)]
struct LogRecord<'a> {
    severity: &'a str,
    message: &'a str,
    fields: LogFields<'a>,
}
struct LogFields<'a>(&'a [host::Pair]);
impl serde::Serialize for LogFields<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        #[derive(serde::Serialize)]
        struct Field<'a> {
            name: &'a str,
            value: &'a str,
        }
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for pair in self.0 {
            sequence.serialize_element(&Field {
                name: &pair.name,
                value: &pair.value,
            })?;
        }
        sequence.end()
    }
}
struct LogLine {
    text: String,
    left: usize,
    invalid: bool,
}
impl std::io::Write for LogLine {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let text = std::str::from_utf8(bytes).map_err(|_| {
            self.invalid = true;
            std::io::ErrorKind::InvalidData
        })?;
        let end = text
            .char_indices()
            .nth(self.left)
            .map(|(at, _)| at)
            .unwrap_or(text.len());
        self.text.push_str(&text[..end]);
        self.left -= text[..end].chars().count();
        if end < text.len() {
            Err(std::io::ErrorKind::WriteZero.into())
        } else {
            Ok(bytes.len())
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Registration<'a> {
    state: &'a State,
    key: (String, String),
    active: Arc<Active>,
}
impl Drop for Registration<'_> {
    fn drop(&mut self) {
        self.state.active.lock().unwrap().remove(&self.key);
        let mut completed = self.active.completed.lock().unwrap();
        if completed.is_none() {
            *completed = Some(false);
        }
        self.active.changed.notify_all();
    }
}

pub(super) fn execute(state: &State, body: &[u8]) -> (u16, Value) {
    let request = match api::parse_request(body) {
        Ok(request) => request,
        Err(api::Rejection::Refused(refused)) => {
            let status = refused.refusal.status();
            return (status, serde_json::to_value(refused.refusal).unwrap());
        }
        Err(_) => return (400, json!({"code":"invalid_request"})),
    };
    let logs = Arc::new(Mutex::new(Vec::new()));
    let bindings = Arc::new(HostBindings {
        _credential: request.credential_for_host(),
        logs: logs.clone(),
    });
    let host = host::Host::with_limits(
        request.host_context(),
        bindings,
        request.execution_limits(state.settings.wall_clock_ms),
    );
    let active = Arc::new(Active {
        cancelled: host.cancellation(),
        completed: Mutex::new(None),
        changed: Condvar::new(),
        sequence: Mutex::new(0),
    });
    let key = request.attempt_key();
    {
        let mut attempts = state.active.lock().unwrap();
        if attempts.contains_key(&key) {
            return (503, serde_json::to_value(api::Refusal::busy()).unwrap());
        }
        attempts.insert(key.clone(), active.clone());
    }
    let _registration = Registration {
        state,
        key,
        active: active.clone(),
    };
    let outcome = invoke(state, &request, host, &active);
    let logs = request.redact_logs(&logs.lock().unwrap());
    let (status, envelope) = match outcome {
        Ok(result) => (200, request.envelope(result, logs, false)),
        Err(code) => {
            let refusal = match code {
                "undeclared_import" => api::Refusal::undeclared_import(),
                "deadline_exceeded" => api::Refusal::timeout(),
                "cancelled" | "compile_cancelled" => api::Refusal::cancelled(),
                "params_too_large" => api::Refusal::payload_too_large(),
                _ => api::Refusal::execution_failed(),
            };
            (refusal.status(), request.refusal_envelope(&refusal, logs))
        }
    };
    match envelope {
        Ok(value) => (status, value),
        Err(_) => (
            500,
            request
                .refusal_envelope(&api::Refusal::execution_failed(), vec![])
                .unwrap_or_else(|_| {
                    serde_json::to_value(api::Refusal::execution_failed()).unwrap()
                }),
        ),
    }
}
fn invoke(
    state: &State,
    request: &api::ExecuteRequest,
    host: host::Host,
    active: &Active,
) -> Result<Value, &'static str> {
    // Preserve host::execute's pre-instantiation input ceiling even though the
    // service prepares separately to publish its guest-start lifecycle event.
    let params = request.params_bytes();
    if params.len() > host::BUFFER_BYTES {
        return Err("params_too_large");
    }
    let projection = request.projection_value();
    if projection["engine_version"] != crate::artifact::cache_key::ENGINE_VERSION {
        return Err("engine_version_mismatch");
    }
    let digest = projection["component_digest"]
        .as_str()
        .ok_or("invalid_request")?;
    let path = state.settings.artifact_directory.join(&digest[7..]);
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|_| "artifact_unavailable")?
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "artifact_unavailable")?;
    super::validate_component(&bytes, request)?;
    if format!("sha256:{:x}", Sha256::digest(&bytes)) != digest {
        return Err("component_digest_mismatch");
    }
    let compile_limits = CompileLimits {
        input_bytes: 64 * 1024 * 1024,
        cpu_seconds: 120,
        memory_bytes: 4 * 1024 * 1024 * 1024,
        wall_clock_deadline: Instant::now() + Duration::from_secs(300),
    };
    let loaded = state
        .cache
        .lock()
        .unwrap()
        .compile(CONTRACT_VERSION, &bytes, compile_limits, &active.cancelled)
        .map_err(|error| error.code)?;
    let prepared = host::Prepared::new(&loaded, host)
        .map_err(|execution| execution.result.unwrap_err().code)?;
    state.event(Listener::Execute, Phase::GuestStarted);
    let execution = prepared.call(&params);
    *active.completed.lock().unwrap() = Some(
        execution
            .result
            .as_ref()
            .is_err_and(|error| error.code == "cancelled"),
    );
    active.changed.notify_all();
    let output = execution.result.map_err(|error| error.code)?;
    serde_json::from_slice(&output).map_err(|_| "invalid_handler_result")
}

pub(super) fn control(state: &State, path: &str, body: &[u8]) -> (u16, Value) {
    let value = match path {
        "/v1/cancel" => api::parse_cancel(body)
            .and_then(|value| serde_json::to_value(value).map_err(Into::into)),
        "/v1/update" => api::parse_update(body)
            .and_then(|value| serde_json::to_value(value).map_err(Into::into)),
        _ => return (404, json!({"code":"not_found"})),
    };
    let Ok(mut value) = value else {
        return (400, json!({"code":"invalid_request"}));
    };
    let key = (
        value["activation_id"].as_str().unwrap().to_owned(),
        value["attempt_id"].as_str().unwrap().to_owned(),
    );
    let active = state.active.lock().unwrap().get(&key).cloned();
    if path == "/v1/update" {
        let Some(active) = active else {
            return (404, json!({"code":"not_found"}));
        };
        let mut sequence = active.sequence.lock().unwrap();
        let next = value["sequence"].as_u64().unwrap();
        if next <= *sequence {
            return (409, json!({"code":"stale_update"}));
        }
        *sequence = next;
        return (200, value);
    }
    if value["cancel"]["state"] != "requested" {
        return (400, json!({"code":"invalid_request"}));
    }
    let acknowledged = active.is_some_and(|active| {
        active.cancelled.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut completed = active.completed.lock().unwrap();
        while completed.is_none() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            completed = active.changed.wait_timeout(completed, remaining).unwrap().0;
        }
        *completed == Some(true)
    });
    value["cancel"]["state"] = json!(if acknowledged {
        "acknowledged"
    } else {
        "uncertain"
    });
    (200, value)
}
