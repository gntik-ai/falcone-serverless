//! The pod-local HTTP service for ctr-worker-execution-api v1: `POST /v1/execute`,
//! `/v1/update` and `/v1/cancel` on the configured `listen_port`.
//!
//! The execution credential lives only in the parsed `ExecuteRequest`, whose `Debug` and
//! `Display` print `<redacted>`. The guest receives the `params` object and the invocation
//! identity through `host::execute`, nothing else: no WASI environment, arguments or
//! preopens exist for a component that may import only `falcone:host/*`. The request is
//! dropped when the attempt ends. Artifact bytes never come from a request: they are read from
//! a trusted read-only directory by digest and verified before compilation.
//!
//! The endpoint does not authenticate its caller yet (INV-01); deployment keeps it reachable
//! only from the dispatcher.

mod attempts;
mod http;
mod log;

use crate::config::{self, RuntimeConfig};
use attempts::Attempts;
use falcone_serverless_worker::{
    api::{self, ExecutionResult, Malformed, Refused, Rejection},
    artifact::{cache_key::ENGINE_VERSION, supervisor::CompileLimits},
    cache::{CONTRACT_VERSION, Cache},
    host::{self, outcome::InvocationOutcome},
    limits::Limits,
};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Where digest-addressed artifacts are mounted read-only unless `--artifacts` says otherwise.
pub const DEFAULT_ARTIFACTS: &str = "/var/run/falcone/artifacts";
const MAX_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CONFIG_BYTES: u64 = 64 * 1024;
const MAX_CONNECTIONS: usize = 64;
const IO_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a peer may take to deliver one request, or to receive an early refusal.
const CONNECTION_DEADLINE: Duration = Duration::from_secs(30);
const CONNECTION_STACK_BYTES: usize = 8 * 1024 * 1024;

/// What the worker answers: an HTTP status and a JSON body.
struct Reply {
    status: u16,
    body: Vec<u8>,
}

/// An answer without an envelope: a fixed label, never a request value.
fn label(status: u16, label: &str) -> Reply {
    Reply {
        status,
        body: serde_json::json!({ "error": label })
            .to_string()
            .into_bytes(),
    }
}

/// The bindings of this worker: none yet, so every host call is denied.
struct Denied;

impl host::Bindings for Denied {
    fn call(&self, _: &host::Context, _: host::Operation) -> Result<host::Reply, host::HostError> {
        Err(host::HostError::Denied)
    }
}

struct Server {
    cache: Mutex<Cache>,
    artifacts: PathBuf,
    attempts: Attempts,
    cancel_wait: Duration,
    connections: AtomicUsize,
}

/// Serves until the process is stopped.
pub fn serve(config_path: &Path, artifacts: &Path) -> Result<(), String> {
    let mut input = String::new();
    std::fs::File::open(config_path)
        .and_then(|file| file.take(MAX_CONFIG_BYTES).read_to_string(&mut input))
        .map_err(|error| format!("read config {}: {error}", config_path.display()))?;
    let config = config::load_from_str(&input)?;
    log::install_panic_hook();
    let server = Arc::new(Server::new(&config, artifacts)?);
    let listener = TcpListener::bind(("0.0.0.0", config.listen_port))
        .map_err(|_| "listen_port: cannot bind".to_owned())?;
    let port = listener.local_addr().map_or(0, |address| address.port());
    log::event("listening", &[("port", &port.to_string())]);
    for stream in listener.incoming().flatten() {
        if server.connections.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
            server.connections.fetch_sub(1, Ordering::AcqRel);
            continue;
        }
        let server = server.clone();
        let spawned = std::thread::Builder::new()
            .stack_size(CONNECTION_STACK_BYTES)
            .spawn({
                let server = server.clone();
                move || {
                    server.connection(stream);
                    server.connections.fetch_sub(1, Ordering::AcqRel);
                }
            });
        if spawned.is_err() {
            server.connections.fetch_sub(1, Ordering::AcqRel);
        }
    }
    Ok(())
}

impl Server {
    fn new(config: &RuntimeConfig, artifacts: &Path) -> Result<Self, String> {
        // The compiler is resolved beside this trusted executable, never from a request or PATH.
        let compiler = std::env::current_exe()
            .map_err(|_| "compile_failed".to_owned())?
            .with_file_name("artifact-compile-child");
        let cache = Cache::new(&config.cache_path, compiler).map_err(|error| error.code)?;
        Ok(Self {
            cache: Mutex::new(cache),
            artifacts: artifacts.to_owned(),
            attempts: Attempts::new(config.concurrency as usize),
            cancel_wait: Duration::from_millis(config.cancel_reserve_ms),
            connections: AtomicUsize::new(0),
        })
    }

    fn connection(&self, mut stream: TcpStream) {
        let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
        let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
        let deadline = Instant::now() + CONNECTION_DEADLINE;
        let mut too_large = false;
        let reply = match http::read_request(&mut stream, deadline) {
            Ok(request) => self.route(&request),
            Err(http::ReadError::Closed) => return,
            Err(http::ReadError::Malformed) => label(400, "malformed_http"),
            Err(http::ReadError::TooLarge) => {
                too_large = true;
                log::event("request_rejected", &[("reason", "body_too_large")]);
                refusal(&Refused {
                    refusal: api::Refusal::body_too_large(),
                    attribution: None,
                })
            }
        };
        http::write_response(&mut stream, reply.status, &reply.body);
        if too_large {
            let _ = stream.shutdown(std::net::Shutdown::Write);
            http::drain(&mut stream, http::MAX_BODY_BYTES * 2, deadline);
        }
    }

    fn route(&self, request: &http::Request) -> Reply {
        let handler: fn(&Self, &[u8]) -> Reply = match request.path.as_str() {
            "/v1/execute" => Self::execute,
            "/v1/update" => Self::update,
            "/v1/cancel" => Self::cancel,
            _ => return label(404, "not_found"),
        };
        if request.method != "POST" {
            return label(405, "method_not_allowed");
        }
        handler(self, &request.body)
    }

    fn execute(&self, body: &[u8]) -> Reply {
        let request = match api::parse_request(body) {
            Ok(request) => request,
            Err(Rejection::Malformed(malformed)) => {
                log::event(
                    "execute_rejected",
                    &[("rejection", &format!("{malformed:?}"))],
                );
                return malformed_reply(&malformed);
            }
            Err(Rejection::Refused(refused)) => {
                log::event("execute_refused", &[("refusal", &format!("{refused:?}"))]);
                return refusal(&refused);
            }
        };
        log::event("execute_admitted", &[("request", &format!("{request:?}"))]);
        let projection = request.projection();
        if projection.engine_version() != ENGINE_VERSION {
            return label(409, "engine_version_mismatch");
        }
        let context = host::Context {
            invocation_key: request.invocation_key().to_owned(),
            attempt_id: request.attempt_key().to_owned(),
            idempotency_key: request.idempotency_key().unwrap_or_default().to_owned(),
            ..host::Context::default()
        };
        let host = host::Host::with_limits(context, Arc::new(Denied), Limits::compatibility());
        let cancel = host.cancellation();
        let running = match self.attempts.admit(
            request.activation_key(),
            request.attempt_key(),
            cancel.clone(),
        ) {
            Ok(running) => running,
            Err(attempts::Refusal::Busy) => return label(503, "busy"),
            Err(attempts::Refusal::Duplicate) => return label(409, "attempt_in_progress"),
        };
        let bytes = match self.artifact(projection.artifact_digest().hex()) {
            Ok(bytes) => bytes,
            Err(reply) => {
                log::event("execute_refused", &[("stage", "artifact")]);
                return reply;
            }
        };
        let limits = CompileLimits {
            input_bytes: MAX_ARTIFACT_BYTES as usize,
            cpu_seconds: 120,
            memory_bytes: 4 * 1024 * 1024 * 1024,
            wall_clock_deadline: Instant::now() + Duration::from_secs(300),
        };
        let compiled = self
            .cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .compile(CONTRACT_VERSION, &bytes, limits, &cancel);
        let loaded = match compiled {
            Ok(loaded) => loaded,
            Err(error) => {
                log::event(
                    "execute_refused",
                    &[("stage", "compile"), ("code", error.code)],
                );
                return label(422, error.code);
            }
        };
        log::event("guest_start", &[]);
        let execution = host::execute(&loaded, host, &request.params_json());
        running.finish(execution.outcome == InvocationOutcome::Uncertain);
        log::event(
            "guest_end",
            &[("outcome", &format!("{:?}", execution.outcome))],
        );
        match execution.result {
            Ok(output) => match ExecutionResult::success(&request, ENGINE_VERSION, &output) {
                Ok(result) => Reply {
                    status: 200,
                    body: serde_json::to_vec(&result).unwrap_or_default(),
                },
                Err(_) => label(502, "invalid_handler_result"),
            },
            // The guest's own error text is never echoed.
            Err(error) => label(
                match error.code {
                    "deadline_exceeded" => 504,
                    "cancelled" => 409,
                    _ => 502,
                },
                error.code,
            ),
        }
    }

    /// The trusted, read-only, digest-addressed artifact source.
    fn artifact(&self, hex: &str) -> Result<Vec<u8>, Reply> {
        let unavailable = || label(404, "artifact_unavailable");
        let path = self.artifacts.join(format!("{hex}.wasm"));
        let metadata = std::fs::symlink_metadata(&path).map_err(|_| unavailable())?;
        if !metadata.is_file() {
            return Err(unavailable());
        }
        let mut bytes = Vec::new();
        std::fs::File::open(&path)
            .and_then(|file| file.take(MAX_ARTIFACT_BYTES + 1).read_to_end(&mut bytes))
            .map_err(|_| unavailable())?;
        if bytes.len() as u64 > MAX_ARTIFACT_BYTES {
            return Err(label(422, "compile_size_limit_exceeded"));
        }
        if format!("{:x}", Sha256::digest(&bytes)) != hex {
            return Err(label(422, "artifact_digest_mismatch"));
        }
        Ok(bytes)
    }

    fn update(&self, body: &[u8]) -> Reply {
        let update = match api::parse_update(body) {
            Ok(update) => update,
            Err(malformed) => return malformed_reply(&malformed),
        };
        if self
            .attempts
            .is_running(update.activation(), update.attempt())
        {
            Reply {
                status: 202,
                body: b"{}".to_vec(),
            }
        } else {
            label(404, "unknown_attempt")
        }
    }

    fn cancel(&self, body: &[u8]) -> Reply {
        let cancel = match api::parse_cancel(body) {
            Ok(cancel) => cancel,
            Err(malformed) => return malformed_reply(&malformed),
        };
        if !cancel.is_requested() {
            return malformed_reply(&Malformed::Constraint("cancel.state"));
        }
        match self
            .attempts
            .cancel(cancel.activation(), cancel.attempt(), self.cancel_wait)
        {
            Some(uncertain) => Reply {
                status: 200,
                body: serde_json::to_vec(&cancel.answer(uncertain)).unwrap_or_default(),
            },
            None => label(404, "unknown_attempt"),
        }
    }
}

fn malformed_reply(malformed: &Malformed) -> Reply {
    log::event(
        "request_rejected",
        &[("rejection", &format!("{malformed:?}"))],
    );
    label(400, &malformed.to_string())
}

fn refusal(refused: &Refused) -> Reply {
    Reply {
        status: refused.refusal.http_status(),
        body: refused.reply(ENGINE_VERSION).unwrap_or_default(),
    }
}
