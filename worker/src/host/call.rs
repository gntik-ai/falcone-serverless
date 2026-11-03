//! Bounded host calls. The deadline and cancellation are enforced by the host thread, never
//! by fuel or epoch interruption, so a stalled binding cannot hold the guest past its deadline.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use super::HostError;
use super::bindings::{CallControl, CancelToken};

/// Binding threads abandoned at a deadline that have not stopped yet. A binding that ignores
/// its control cannot make the host spawn without bound.
const MAX_LIVE_CALLS: usize = 4;
const POLL: Duration = Duration::from_millis(5);

struct Live(Arc<AtomicUsize>);

impl Drop for Live {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Runs `call` on a helper thread and waits for it until `timeout` or cancellation.
pub fn run_bounded<T, F>(
    live: &Arc<AtomicUsize>,
    cancel: &CancelToken,
    timeout: Duration,
    call: F,
) -> Result<T, HostError>
where
    T: Send + 'static,
    F: FnOnce(&CallControl) -> Result<T, HostError> + Send + 'static,
{
    if cancel.is_cancelled() {
        return Err(HostError::Cancelled);
    }
    if live.fetch_add(1, Ordering::AcqRel) >= MAX_LIVE_CALLS {
        live.fetch_sub(1, Ordering::AcqRel);
        return Err(HostError::LimitExceeded);
    }
    let guard = Live(Arc::clone(live));
    let deadline = Instant::now() + timeout;
    let control = CallControl::new(cancel.clone(), deadline);
    let (sender, receiver) = mpsc::channel();
    let worker_control = control.clone();
    let spawned = thread::Builder::new()
        .name("falcone-host-call".into())
        .spawn(move || {
            let _guard = guard;
            let _ = sender.send(call(&worker_control));
        });
    if spawned.is_err() {
        // The guard moved into the failed closure and was dropped with it.
        return Err(HostError::LimitExceeded);
    }
    loop {
        let wait = control.remaining().min(POLL);
        match receiver.recv_timeout(wait) {
            Ok(result) => return result,
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(HostError::Denied),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if cancel.is_cancelled() {
            control.expire();
            return Err(HostError::Cancelled);
        }
        if Instant::now() >= deadline {
            control.expire();
            return Err(HostError::Timeout);
        }
    }
}
