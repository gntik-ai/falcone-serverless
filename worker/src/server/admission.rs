//! Connection admission with separate execute capacity and a fixed control reserve.
//!
//! At most `concurrency` execute requests run at once. A cancel or update takes one of `reserve`
//! control slots, which an execute request never uses, so a cancel can be served exactly when
//! every execute slot is held by a blocked guest. The connection limit is derived from both:
//! `concurrency + reserve` bounds the connections that hold an execute or a control slot.
//!
//! A connection that holds neither is not counted against that limit, so it cannot take the
//! capacity a cancel needs. There are three such places, each a small bounded pool whose oldest
//! member is closed to make room for a newer one:
//!
//! - `Head`: a new connection, until its request line and headers are read. Its route is not
//!   known yet, so no capacity can be set aside for a cancel; instead a silent or dripping peer
//!   is replaced by the next connection, which reads its head at once. Size: the connection limit.
//! - `Overflow`: an execute request that found every execute slot taken. The contract refuses a
//!   `runtime_mode` other than `wasm` before capacity is considered, which needs its body, so the
//!   body is read here (ceilings and deadlines unchanged) and the reply is the refusal or `busy`.
//!   Size: `reserve`.
//! - `Body`: a cancel or update holding a control slot while its (small) body arrives. Size: the
//!   control reserve itself, so a stalled control body is replaced by a newer control request
//!   rather than starving it.
//!
//! A request that is running (executing, or a control request past its body) is never replaced.

use std::net::{Shutdown, TcpStream};
use std::sync::{Arc, Mutex, PoisonError};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Head,
    Overflow,
    Body,
}

/// A connection that can be closed to make room, oldest first.
struct Watched {
    id: u64,
    stage: Stage,
    stream: TcpStream,
}

#[derive(Default)]
struct State {
    next: u64,
    executes: usize,
    controls: usize,
    watched: Vec<Watched>,
}

impl State {
    fn count(&self, stage: Stage) -> usize {
        self.watched.iter().filter(|w| w.stage == stage).count()
    }

    /// Registers a connection in `stage`.
    fn watch(&mut self, id: u64, stage: Stage, stream: &TcpStream) -> bool {
        match stream.try_clone() {
            Ok(stream) => {
                self.watched.push(Watched { id, stage, stream });
                true
            }
            Err(_) => false,
        }
    }

    /// Stops watching `id`; whether it was still watched (it is not once it was replaced).
    fn unwatch(&mut self, id: u64) -> Option<Stage> {
        let at = self.watched.iter().position(|w| w.id == id)?;
        Some(self.watched.remove(at).stage)
    }

    /// Closes the oldest connection in `stage`, if any, and reports whether one was closed. The
    /// connection is accounted as gone at once; its thread sees a closed socket and ends.
    fn replace_oldest(&mut self, stage: Stage) -> bool {
        let Some(at) = self.watched.iter().position(|w| w.stage == stage) else {
            return false;
        };
        let victim = self.watched.remove(at);
        let _ = victim.stream.shutdown(Shutdown::Both);
        if stage == Stage::Body {
            self.controls -= 1;
        }
        true
    }
}

pub struct Admission {
    concurrency: usize,
    reserve: usize,
    state: Mutex<State>,
}

/// What a request may do once its route is known.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Admit {
    /// A slot is held: read the body and run.
    Run,
    /// An execute request over capacity: read the body, then answer without running.
    Excess,
    /// A control request with the whole reserve running: answer `busy` at once.
    Busy,
    /// The connection was replaced meanwhile: close it.
    Gone,
}

impl Admission {
    pub fn new(concurrency: u32, reserve: u32) -> Arc<Self> {
        Arc::new(Self {
            concurrency: concurrency.max(1) as usize,
            reserve: reserve.max(1) as usize,
            state: Mutex::new(State::default()),
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The connection limit: execute capacity plus the control reserve.
    pub fn connection_limit(&self) -> usize {
        self.concurrency + self.reserve
    }

    /// A handle for one accepted connection, which starts in the head pool. When the pool is
    /// full its oldest member is closed. `None` only when the socket cannot be watched.
    pub fn connect(self: &Arc<Self>, stream: &TcpStream) -> Option<Connection> {
        let mut state = self.state();
        if state.count(Stage::Head) >= self.connection_limit() {
            state.replace_oldest(Stage::Head);
        }
        let id = state.next;
        state.next += 1;
        if !state.watch(id, Stage::Head, stream) {
            return None;
        }
        Some(Connection {
            admission: self.clone(),
            id,
            held: Held::Head,
            stream: stream.try_clone().ok()?,
        })
    }

    /// Connections still reading their head.
    pub fn heads(&self) -> usize {
        self.state().count(Stage::Head)
    }

    /// Excess execute requests being read outside the connection count.
    pub fn overflow(&self) -> usize {
        self.state().count(Stage::Overflow)
    }

    /// `(connections, executes, controls)` in use: the connections holding a slot.
    pub fn slots(&self) -> (usize, usize, usize) {
        let state = self.state();
        (
            state.executes + state.controls,
            state.executes,
            state.controls,
        )
    }
}

enum Held {
    /// Watched in the head pool.
    Head,
    /// Watched in the overflow pool.
    Overflow,
    /// An execute slot.
    Execute,
    /// A control slot, watched while its body arrives.
    ControlBody,
    /// A control slot, running.
    Control,
}

/// One accepted connection; everything it holds is released on drop.
pub struct Connection {
    admission: Arc<Admission>,
    id: u64,
    held: Held,
    /// A handle on the socket, so a later pool can watch it.
    stream: TcpStream,
}

impl Connection {
    /// Leaves the head pool for execute capacity, or for the overflow pool when every execute
    /// slot is taken. An execute request never takes a control slot.
    pub fn route_execute(&mut self) -> Admit {
        let admission = self.admission.clone();
        let mut state = admission.state();
        if state.unwatch(self.id).is_none() {
            return Admit::Gone;
        }
        if state.executes < admission.concurrency {
            state.executes += 1;
            self.held = Held::Execute;
            return Admit::Run;
        }
        if state.count(Stage::Overflow) >= admission.reserve {
            state.replace_oldest(Stage::Overflow);
        }
        if !state.watch(self.id, Stage::Overflow, &self.stream) {
            return Admit::Gone;
        }
        self.held = Held::Overflow;
        Admit::Excess
    }

    /// Leaves the head pool for a control slot. When the whole reserve is taken, the oldest
    /// control request still waiting for its body is replaced; `Busy` only if every control
    /// request is running.
    pub fn route_control(&mut self) -> Admit {
        let admission = self.admission.clone();
        let mut state = admission.state();
        if state.unwatch(self.id).is_none() {
            return Admit::Gone;
        }
        if state.controls >= admission.reserve && !state.replace_oldest(Stage::Body) {
            // The reply is sent from the head pool, so it still counts there.
            return if state.watch(self.id, Stage::Head, &self.stream) {
                self.held = Held::Head;
                Admit::Busy
            } else {
                Admit::Gone
            };
        }
        if !state.watch(self.id, Stage::Body, &self.stream) {
            return Admit::Gone;
        }
        state.controls += 1;
        self.held = Held::ControlBody;
        Admit::Run
    }

    /// The body of a control request is complete: it is now a running request and is no longer
    /// replaced. `false` when it was replaced in the meantime.
    pub fn body_read(&mut self) -> bool {
        if !matches!(self.held, Held::ControlBody) {
            return true;
        }
        let mut state = self.admission.state();
        if state.unwatch(self.id).is_none() {
            return false;
        }
        self.held = Held::Control;
        true
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let mut state = self.admission.state();
        match self.held {
            Held::Head | Held::Overflow => {
                state.unwatch(self.id);
            }
            Held::Execute => state.executes -= 1,
            Held::ControlBody => {
                // A replaced request was accounted for when it was replaced.
                if state.unwatch(self.id).is_some() {
                    state.controls -= 1;
                }
            }
            Held::Control => state.controls -= 1,
        }
    }
}
