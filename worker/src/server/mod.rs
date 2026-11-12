//! Pod-local execution transport. Request data is never used in diagnostics.
mod attempt;
mod execution;
mod http;

use crate::{api, config::RuntimeConfig};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

/// Fixed control reserve, independent of the execution concurrency.
pub const CONTROL_RESERVE: usize = 4;

/// A separate pod-local listener protects control admission even before a request
/// sends its first byte. Keep the closed RuntimeConfig v1 schema unchanged.
pub fn control_port(listen_port: u16) -> Result<u16, &'static str> {
    listen_port.checked_add(2).ok_or("invalid_control_port")
}

struct Permit(Arc<AtomicUsize>);
impl Permit {
    fn acquire(count: &Arc<AtomicUsize>, limit: usize) -> Option<Self> {
        count
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < limit).then_some(n + 1)
            })
            .ok()?;
        Some(Self(count.clone()))
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// One process-owned service, shared by accepted HTTP connections.
pub struct Service {
    config: RuntimeConfig,
    connections: Arc<AtomicUsize>,
    controls: Arc<AtomicUsize>,
    cache: Mutex<Option<crate::cache::Cache>>,
    attempts: attempt::Attempts,
}
impl Service {
    pub fn new(config: RuntimeConfig) -> Result<Arc<Self>, &'static str> {
        if config.concurrency == 0 {
            return Err("invalid_concurrency");
        }
        Ok(Arc::new(Self {
            config,
            connections: Arc::new(AtomicUsize::new(0)),
            controls: Arc::new(AtomicUsize::new(0)),
            cache: Mutex::new(None),
            attempts: Mutex::new(Default::default()),
        }))
    }
    pub fn connection(self: &Arc<Self>) -> Option<Connection> {
        self.admit(false)
    }
    /// Only cancel/update may use this listener's reserved capacity.
    pub fn control_connection(self: &Arc<Self>) -> Option<Connection> {
        self.admit(true)
    }
    fn admit(self: &Arc<Self>, control: bool) -> Option<Connection> {
        let (count, limit) = if control {
            (&self.controls, CONTROL_RESERVE)
        } else {
            (&self.connections, self.config.concurrency as usize)
        };
        let permit = Permit::acquire(count, limit)?;
        Some(Connection {
            service: self.clone(),
            _permit: permit,
            control,
        })
    }
    fn dispatch(&self, path: &str, body: &[u8]) -> (u16, Value) {
        match path {
            "/v1/execute" => match api::parse_request(body) {
                Err(api::Rejection::Refused(refused)) => {
                    let value = serde_json::to_value(refused.refusal).unwrap_or(Value::Null);
                    (value["status"].as_u64().unwrap_or(400) as u16, value)
                }
                Err(api::Rejection::Malformed(_)) => error(400, "invalid_request"),
                Ok(request) => {
                    execution::execute(self, request).unwrap_or_else(|response| response)
                }
            },
            "/v1/cancel" => match api::parse_cancel(body) {
                Ok(cancel) if cancel.is_requested() => {
                    let attempt = self
                        .attempts
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .get(&cancel.key())
                        .cloned();
                    let acknowledged = attempt.is_some_and(|attempt| {
                        attempt.cancel(Duration::from_millis(self.config.cancel_reserve_ms))
                    });
                    (
                        200,
                        serde_json::to_value(cancel.reply(acknowledged)).unwrap_or(Value::Null),
                    )
                }
                _ => error(400, "invalid_request"),
            },
            "/v1/update" => match api::parse_update(body) {
                Ok(update) => {
                    let attempt = self
                        .attempts
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .get(&update.key())
                        .cloned();
                    match attempt {
                        Some(attempt) => (
                            if attempt.running.load(Ordering::Acquire) {
                                200
                            } else {
                                202
                            },
                            serde_json::to_value(update).unwrap_or(Value::Null),
                        ),
                        None => error(400, "invalid_request"),
                    }
                }
                Err(_) => error(400, "invalid_request"),
            },
            _ => error(400, "invalid_request"),
        }
    }
}

/// An accepted connection owns exactly one transport slot until its single response.
pub struct Connection {
    service: Arc<Service>,
    _permit: Permit,
    control: bool,
}
impl Connection {
    pub fn respond(self, io: &mut (impl Read + Write), diagnostics: &mut dyn Write) {
        let response = (|| {
            let (path, length) = http::head(io, self.control)?;
            let body = http::body(io, length)?;
            Ok(self.service.dispatch(&path, &body))
        })()
        .unwrap_or_else(|response| response);
        let _ = writeln!(
            diagnostics,
            "{}",
            json!({"event":"worker_http", "status":response.0})
        );
        let _ = http::write(io, response);
    }
}

fn error(status: u16, code: &'static str) -> (u16, Value) {
    (status, json!({"code":code}))
}

/// Serve on the pod's execute and derived control ports. Errors contain no values.
pub fn serve(config: RuntimeConfig) -> Result<(), &'static str> {
    let control_port = control_port(config.listen_port)?;
    let listener =
        TcpListener::bind(("0.0.0.0", config.listen_port)).map_err(|_| "listen_failed")?;
    let controls = TcpListener::bind(("0.0.0.0", control_port)).map_err(|_| "listen_failed")?;
    let service = Service::new(config)?;
    std::thread::scope(|scope| {
        let executions = service.clone();
        std::thread::Builder::new()
            .name("worker-execute-listener".into())
            .spawn_scoped(scope, move || accept(listener, executions, false))
            .map_err(|_| "listen_failed")?;
        accept(controls, service, true);
        Ok(())
    })
}

fn connection_error() {
    // Never format OS errors or peer/request values in diagnostics.
    let _ = writeln!(
        std::io::stderr(),
        "{{\"event\":\"worker_connection_failed\"}}"
    );
}

fn accept(listener: TcpListener, service: Arc<Service>, control: bool) {
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(_) => {
                connection_error();
                // Resource exhaustion must not turn the accept loop into a spin loop.
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
        };
        if stream.set_write_timeout(Some(http::IO_TIMEOUT)).is_err() {
            connection_error();
            continue;
        }
        let mut stream = http::Socket::new(stream);
        if let Some(connection) = service.admit(control) {
            if std::thread::Builder::new()
                .name("worker-http".into())
                .spawn(move || {
                    connection.respond(&mut stream, &mut std::io::stderr());
                })
                .is_err()
            {
                connection_error();
            }
        } else {
            let _ = http::write(&mut stream, error(503, "busy"));
        }
    }
}
