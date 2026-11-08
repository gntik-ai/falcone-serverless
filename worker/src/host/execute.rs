//! Bounded guest execution through the exported `falcone:host/handler@0.2.0` interface.
//!
//! The caller supplies a component that the trusted compiler or cache path already built
//! after artifact validation; nothing here compiles, loads or trusts native tenant bytes.
//! Linking reuses the deny-by-default capability host, so a guest reaches the platform only
//! through the versioned `falcone:host` interfaces its grants allow. Every outcome is
//! either the complete result or one stable, bounded failure code, never partial output.
use super::runtime::instantiate_component;
use super::{BUFFER_BYTES, Bindings, Context, Host, HostError, Operation, Reply};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use wasmtime::component::{Component, ComponentType, Lift, WasmList, WasmStr};
use wasmtime::{Config, Engine, Trap, UpdateDeadline};

/// The one instance a component exports; the validator requires exactly this name.
pub const HANDLER_INSTANCE: &str = "falcone:host/handler@0.2.0";
/// Guest strings in `handler-error` are cut to this many bytes (on a character boundary).
pub const MAX_ERROR_MESSAGE_BYTES: usize = 256;
/// Epoch granularity: deadline and cancellation are observed at most this late.
pub const EPOCH_TICK: Duration = Duration::from_millis(10);

/// Per-invocation budgets. Both are mandatory: policy values belong to the caller.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Wall-clock allowance from the start of execution, enforced by epoch interruption.
    pub deadline: Duration,
    /// Wasm fuel for the whole invocation; `u64::MAX` leaves the deadline as the CPU bound.
    pub fuel: u64,
}

/// One failure: a stable snake_case code and, for handler errors, a bounded guest message.
#[derive(Debug, PartialEq, Eq)]
pub struct Failure {
    pub code: &'static str,
    pub message: Option<String>,
}

impl Failure {
    fn new(code: &'static str) -> Self {
        Self {
            code,
            message: None,
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.message {
            // Debug formatting escapes control characters from guest-supplied text.
            Some(message) => write!(f, "{}: {message:?}", self.code),
            None => f.write_str(self.code),
        }
    }
}

#[derive(ComponentType, Lift)]
#[component(variant)]
enum HandlerError {
    #[component(name = "invalid-input")]
    InvalidInput(WasmStr),
    #[component(name = "failed")]
    Failed(WasmStr),
}

/// Why the epoch callback stopped a guest.
#[derive(Debug)]
enum Stop {
    Timeout,
    Cancelled,
}

impl std::fmt::Display for Stop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Timeout => "guest deadline reached",
            Self::Cancelled => "invocation cancelled",
        })
    }
}

impl std::error::Error for Stop {}

/// The pinned engine configuration. Keep in sync with `ENGINE_CONFIG` in the artifact cache
/// key and with the trusted compiler child: all other options are the engine defaults.
pub fn engine() -> Result<Engine, Failure> {
    let mut config = Config::new();
    config.consume_fuel(true).epoch_interruption(true);
    Engine::new(&config).map_err(|_| Failure::new("engine_unavailable"))
}

/// Platform bindings for qualification runs: logs and progress are dropped and every other
/// operation is denied. No grants are configured with it, so only the ungated interfaces link.
pub struct Unbound;

impl Bindings for Unbound {
    fn call(&self, _context: &Context, operation: Operation) -> Result<Reply, HostError> {
        match operation {
            Operation::Log { .. } | Operation::Progress { .. } => Ok(Reply::Empty),
            _ => Err(HostError::Denied),
        }
    }
}

/// Owns an epoch ticker for its engine, so every store on that engine shares one clock while
/// each store keeps its own deadline. Dropping the executor stops the ticker.
pub struct Executor {
    engine: Engine,
    stop: Arc<AtomicBool>,
    ticker: Option<JoinHandle<()>>,
}

impl Executor {
    /// `engine` must be configured with fuel and epoch interruption, as [`engine`] does, and
    /// must be the engine that built every component passed to [`Executor::execute`].
    pub fn new(engine: Engine) -> Result<Self, Failure> {
        let stop = Arc::new(AtomicBool::new(false));
        let ticker = {
            let (engine, stop) = (engine.clone(), stop.clone());
            std::thread::Builder::new()
                .name("epoch-ticker".into())
                .spawn(move || {
                    while !stop.load(Ordering::Acquire) {
                        std::thread::sleep(EPOCH_TICK);
                        engine.increment_epoch();
                    }
                })
                .map_err(|_| Failure::new("executor_unavailable"))?
        };
        Ok(Self {
            engine,
            stop,
            ticker: Some(ticker),
        })
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Run `handle(params)` in a fresh store.
    ///
    /// `params` above [`BUFFER_BYTES`] fail before the guest is entered, so none of it is
    /// written to guest memory. A result above the ceiling fails after its length is read and
    /// before it is copied. A deadline, cancellation, trap or handler error is one coded
    /// [`Failure`]; a cancelled invocation never returns output even if the guest finished.
    pub fn execute(
        &self,
        component: &Component,
        mut host: Host,
        params: &[u8],
        limits: Limits,
    ) -> Result<Vec<u8>, Failure> {
        if params.len() > BUFFER_BYTES {
            return Err(Failure::new("limit_exceeded"));
        }
        let cancelled = host.cancellation();
        if cancelled.load(Ordering::Acquire) {
            return Err(Failure::new("cancelled"));
        }
        let deadline = Instant::now() + limits.deadline;
        // A blocking host call must not outlive the guest deadline.
        host.set_call_timeout(limits.deadline);
        let observed = cancelled.clone();
        let (mut store, instance) =
            instantiate_component(&self.engine, component, host, move |store| {
                store.set_fuel(limits.fuel)?;
                // One tick at a time: each tick re-checks cancellation and the deadline, so
                // this store stops without disturbing others on the shared engine.
                store.epoch_deadline_callback(move |_| {
                    if observed.load(Ordering::Acquire) {
                        return Err(wasmtime::Error::new(Stop::Cancelled));
                    }
                    if Instant::now() >= deadline {
                        return Err(wasmtime::Error::new(Stop::Timeout));
                    }
                    Ok(UpdateDeadline::Continue(1))
                });
                store.set_epoch_deadline(1);
                Ok(())
            })
            .map_err(|error| classify(&error, "instantiate_failed"))?;
        let export = instance
            .get_export_index(&mut store, None, HANDLER_INSTANCE)
            .ok_or_else(|| Failure::new("missing_handler_export"))?;
        let handle = instance
            .get_export_index(&mut store, Some(&export), "handle")
            .ok_or_else(|| Failure::new("missing_handler_export"))?;
        let handle = instance
            .get_typed_func::<(&[u8],), (Result<WasmList<u8>, HandlerError>,)>(&mut store, handle)
            .map_err(|_| Failure::new("missing_handler_export"))?;
        let (result,) = handle
            .call(&mut store, (params,))
            .map_err(|error| classify(&error, "trap"))?;
        let output = match result {
            Ok(list) => {
                // Lazy list: the length is known, and checked, before any element is copied.
                if list.len() > BUFFER_BYTES {
                    return Err(Failure::new("limit_exceeded"));
                }
                list.as_le_slice(&store).to_vec()
            }
            Err(HandlerError::InvalidInput(message)) => {
                return Err(handler_failure(
                    "handler_invalid_input",
                    &message.to_str(&store),
                ));
            }
            Err(HandlerError::Failed(message)) => {
                return Err(handler_failure("handler_failed", &message.to_str(&store)));
            }
        };
        if cancelled.load(Ordering::Acquire) {
            return Err(Failure::new("cancelled"));
        }
        Ok(output)
    }
}

impl Drop for Executor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(ticker) = self.ticker.take() {
            let _ = ticker.join();
        }
    }
}

fn classify(error: &wasmtime::Error, fallback: &'static str) -> Failure {
    Failure::new(match error.downcast_ref::<Stop>() {
        Some(Stop::Timeout) => "timeout",
        Some(Stop::Cancelled) => "cancelled",
        None => match error.downcast_ref::<Trap>() {
            Some(Trap::OutOfFuel) => "limit_exceeded",
            Some(Trap::Interrupt) => "timeout",
            Some(_) => "trap",
            None => fallback,
        },
    })
}

fn handler_failure(
    code: &'static str,
    message: &wasmtime::Result<std::borrow::Cow<'_, str>>,
) -> Failure {
    Failure {
        code,
        // A string that is not valid UTF-8 carries no message; the code still says why.
        message: message
            .as_ref()
            .ok()
            .map(|message| truncate(message, MAX_ERROR_MESSAGE_BYTES)),
    }
}

fn truncate(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::truncate;

    #[test]
    fn truncation_never_splits_a_character() {
        assert_eq!(truncate("abc", 256), "abc");
        assert_eq!(truncate("abcdef", 3), "abc");
        // "é" is two bytes: cutting at 3 would split the second one.
        assert_eq!(truncate("aéé", 3), "aé");
        assert_eq!(truncate("éé", 1), "");
        assert_eq!(truncate("", 0), "");
    }
}
