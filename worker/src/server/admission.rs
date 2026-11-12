//! Connection admission with separate execute capacity and a fixed control reserve.
//!
//! At most `concurrency` execute requests run at once. A cancel or update takes one of `reserve`
//! control slots, which an execute request never uses, so a cancel can be served exactly when
//! every execute slot is held by a blocked guest. The connection limit is derived from both:
//! `concurrency + reserve`. A connection over the limit is closed at accept, before a byte is
//! read; a control request over its own capacity is answered `busy` at once.
//!
//! An execute request over capacity is answered `busy` too, but the contract refuses a
//! `runtime_mode` other than `wasm` first, which needs its body. Such a request is moved out of
//! the connection count into a small `overflow` pool of `reserve` readers (body ceilings and
//! deadlines unchanged) while it is read, so no number of excess execute requests, however slowly
//! they send, can occupy a connection a cancel needs. Over the pool it is answered `busy` at once
//! without reading its body.

use std::sync::{Arc, Mutex, PoisonError};

#[derive(Default)]
struct State {
    connections: usize,
    executes: usize,
    controls: usize,
    overflow: usize,
}

pub struct Admission {
    concurrency: usize,
    reserve: usize,
    state: Mutex<State>,
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

    /// A slot for one accepted connection, or `None` at the limit.
    pub fn connect(self: &Arc<Self>) -> Option<Connection> {
        let mut state = self.state();
        if state.connections >= self.connection_limit() {
            return None;
        }
        state.connections += 1;
        Some(Connection {
            admission: self.clone(),
            execute: false,
            control: false,
            overflow: false,
        })
    }

    /// Excess execute requests being read outside the connection count.
    pub fn overflow(&self) -> usize {
        self.state().overflow
    }

    /// `(connections, executes, controls)` in use.
    pub fn slots(&self) -> (usize, usize, usize) {
        let state = self.state();
        (state.connections, state.executes, state.controls)
    }
}

/// One admitted connection; everything it holds is released on drop.
pub struct Connection {
    admission: Arc<Admission>,
    execute: bool,
    control: bool,
    overflow: bool,
}

impl Connection {
    /// Claims execute capacity; `false` when `concurrency` executions are running.
    pub fn take_execute(&mut self) -> bool {
        let mut state = self.admission.state();
        if self.execute || state.executes >= self.admission.concurrency {
            return false;
        }
        state.executes += 1;
        self.execute = true;
        true
    }

    /// Moves an execute request that found no capacity out of the connection count into the
    /// overflow pool, which a control request never needs. `false` when the pool is full.
    pub fn take_overflow(&mut self) -> bool {
        let mut state = self.admission.state();
        if self.overflow || self.execute || state.overflow >= self.admission.reserve {
            return false;
        }
        state.connections -= 1;
        state.overflow += 1;
        self.overflow = true;
        true
    }

    /// Claims a control slot; `false` when the whole reserve is in use.
    pub fn take_control(&mut self) -> bool {
        let mut state = self.admission.state();
        if self.control || state.controls >= self.admission.reserve {
            return false;
        }
        state.controls += 1;
        self.control = true;
        true
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let mut state = self.admission.state();
        if self.overflow {
            state.overflow -= 1;
        } else {
            state.connections -= 1;
        }
        if self.execute {
            state.executes -= 1;
        }
        if self.control {
            state.controls -= 1;
        }
    }
}
