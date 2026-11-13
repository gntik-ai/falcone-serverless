//! Pod-local execution service. Components are raw bytes staged by the platform.
mod http;
mod run;

use crate::{api, cache::Cache};
use std::collections::BTreeMap;
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc,
};
use std::thread::JoinHandle;
use std::time::Duration;

pub const EXECUTE_ADMISSION: usize = 4;
pub const CONTROL_CONNECTIONS: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Listener {
    Execute,
    Control,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    Accepted,
    Headers,
    Body,
    GuestStarted,
    ResponseStarted,
    Closed,
}
/// Bounded, value-free lifecycle telemetry. Delivery never blocks a listener.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Event {
    pub listener: Listener,
    pub phase: Phase,
}

pub struct Settings {
    pub execute_addr: SocketAddr,
    pub control_addr: SocketAddr,
    pub concurrency: usize,
    pub wall_clock_ms: u64,
    pub artifact_directory: PathBuf,
    pub cache_directory: PathBuf,
    pub compiler: PathBuf,
}

struct Active {
    cancelled: Arc<AtomicBool>,
    completed: Mutex<Option<bool>>,
    changed: Condvar,
    sequence: Mutex<u64>,
}
struct State {
    settings: Settings,
    cache: Mutex<Cache>,
    stop: AtomicBool,
    connections: Mutex<[usize; 2]>,
    idle: Condvar,
    executions: AtomicUsize,
    active: Mutex<BTreeMap<(String, String), Arc<Active>>>,
    events: mpsc::SyncSender<Event>,
}
impl State {
    fn event(&self, listener: Listener, phase: Phase) {
        let _ = self.events.try_send(Event { listener, phase });
    }
    fn connection(self: &Arc<Self>, listener: Listener) -> Option<Connection> {
        let mut counts = self.connections.lock().unwrap();
        let (index, max) = match listener {
            Listener::Execute => (0, self.settings.concurrency + EXECUTE_ADMISSION),
            Listener::Control => (1, CONTROL_CONNECTIONS),
        };
        if counts[index] >= max {
            return None;
        }
        counts[index] += 1;
        self.event(listener, Phase::Accepted);
        Some(Connection {
            state: self.clone(),
            listener,
            index,
        })
    }
    fn execution(&self) -> Option<ExecutionPermit<'_>> {
        self.executions
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < self.settings.concurrency).then_some(count + 1)
            })
            .ok()
            .map(|_| ExecutionPermit(self))
    }
}
struct Connection {
    state: Arc<State>,
    listener: Listener,
    index: usize,
}
impl Drop for Connection {
    fn drop(&mut self) {
        let mut counts = self.state.connections.lock().unwrap();
        counts[self.index] -= 1;
        self.state.event(self.listener, Phase::Closed);
        self.state.idle.notify_all();
    }
}
struct ExecutionPermit<'a>(&'a State);
impl Drop for ExecutionPermit<'_> {
    fn drop(&mut self) {
        self.0.executions.fetch_sub(1, Ordering::AcqRel);
    }
}

pub struct Server {
    state: Arc<State>,
    execute_addr: SocketAddr,
    control_addr: SocketAddr,
    listeners: Vec<JoinHandle<()>>,
    events: mpsc::Receiver<Event>,
}
impl Server {
    pub fn start(settings: Settings) -> Result<Self, &'static str> {
        if !settings.execute_addr.ip().is_loopback()
            || !settings.control_addr.ip().is_loopback()
            || !(1..=1024).contains(&settings.concurrency)
            || settings.wall_clock_ms == 0
        {
            return Err("invalid_service_config");
        }
        let execute =
            TcpListener::bind(settings.execute_addr).map_err(|_| "listener_unavailable")?;
        let control =
            TcpListener::bind(settings.control_addr).map_err(|_| "listener_unavailable")?;
        let execute_addr = execute.local_addr().map_err(|_| "listener_unavailable")?;
        let control_addr = control.local_addr().map_err(|_| "listener_unavailable")?;
        execute
            .set_nonblocking(true)
            .map_err(|_| "listener_unavailable")?;
        control
            .set_nonblocking(true)
            .map_err(|_| "listener_unavailable")?;
        let cache = Cache::new(&settings.cache_directory, &settings.compiler)
            .map_err(|_| "trusted_cache_unavailable")?;
        let (sender, events) = mpsc::sync_channel(1024);
        let state = Arc::new(State {
            settings,
            cache: Mutex::new(cache),
            stop: AtomicBool::new(false),
            connections: Mutex::new([0; 2]),
            idle: Condvar::new(),
            executions: AtomicUsize::new(0),
            active: Mutex::new(BTreeMap::new()),
            events: sender,
        });
        let mut listeners = Vec::new();
        for (socket, listener) in [(execute, Listener::Execute), (control, Listener::Control)] {
            let state = state.clone();
            listeners.push(std::thread::spawn(move || {
                while !state.stop.load(Ordering::Acquire) {
                    match socket.accept() {
                        Ok((stream, _)) => {
                            if let Some(permit) = state.connection(listener) {
                                let state = state.clone();
                                // Failure to spawn drops the captured socket and permit.
                                let _ = std::thread::Builder::new()
                                    .name("worker-http".into())
                                    .spawn(move || http::serve(stream, state, permit));
                            } else {
                                http::busy(stream);
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::park_timeout(Duration::from_millis(2))
                        }
                        Err(_) => break,
                    }
                }
            }));
        }
        Ok(Self {
            state,
            execute_addr,
            control_addr,
            listeners,
            events,
        })
    }
    pub fn execute_addr(&self) -> SocketAddr {
        self.execute_addr
    }
    pub fn control_addr(&self) -> SocketAddr {
        self.control_addr
    }
    pub fn next_event(&self, timeout: Duration) -> Option<Event> {
        self.events.recv_timeout(timeout).ok()
    }
    pub fn shutdown(&mut self) {
        self.state.stop.store(true, Ordering::Release);
        for active in self.state.active.lock().unwrap().values() {
            active.cancelled.store(true, Ordering::Release);
        }
        for listener in self.listeners.drain(..) {
            let _ = listener.join();
        }
        let mut counts = self.state.connections.lock().unwrap();
        while *counts != [0, 0] {
            counts = self.state.idle.wait(counts).unwrap();
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Admission must precede compiler startup, cache lookup and guest start code.
pub fn validate_component(bytes: &[u8], request: &api::ExecuteRequest) -> Result<(), &'static str> {
    let component =
        crate::artifact::validate::validate(bytes, 64 * 1024 * 1024).map_err(|error| error.code)?;
    let projection = request.projection_value();
    let declared = projection["imports"].as_array().ok_or("invalid_request")?;
    if component
        .imports
        .iter()
        .any(|import| !declared.iter().any(|value| value.as_str() == Some(import)))
    {
        return Err("undeclared_import");
    }
    Ok(())
}
