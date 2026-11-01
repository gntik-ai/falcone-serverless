use std::io::{self, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Resource, Rlimit, prlimit};

use super::Error;
use super::cache_key::{CacheKey, ENGINE_CONFIG, ENGINE_VERSION, INTERFACE_VERSION};
use super::validate::validate;

/// All budgets are mandatory; numeric production ceilings belong to the profile.
#[derive(Clone, Copy)]
pub struct CompileLimits {
    pub input_bytes: usize,
    pub cpu_seconds: u64,
    /// RLIMIT_AS: virtual address space, not resident memory.
    pub memory_bytes: u64,
    pub wall_clock_deadline: Instant,
}

pub struct Supervisor {
    child_path: PathBuf,
    spawned: Arc<AtomicUsize>,
    last_child: Option<Child>,
}

impl Supervisor {
    /// The executable is a trusted platform binary, never a tenant-supplied path.
    pub fn new(child_path: impl Into<PathBuf>) -> Self {
        Self {
            child_path: child_path.into(),
            spawned: Arc::new(AtomicUsize::new(0)),
            last_child: None,
        }
    }

    pub fn spawn_count(&self) -> usize {
        self.spawned.load(Ordering::Acquire)
    }

    pub fn spawn_counter(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.spawned)
    }

    /// Uses Child::try_wait so callers can observe the reap guarantee.
    pub fn last_child_status(&mut self) -> io::Result<Option<ExitStatus>> {
        match self.last_child.as_mut() {
            Some(child) => child.try_wait(),
            None => Ok(None),
        }
    }

    pub fn compile(
        &mut self,
        bytes: &[u8],
        limits: CompileLimits,
        cancelled: &AtomicBool,
    ) -> Result<CacheKey, Error> {
        let validated = validate(bytes, limits.input_bytes)?;
        if limits.input_bytes == 0
            || limits.cpu_seconds == 0
            || limits.cpu_seconds == u64::MAX
            || limits.memory_bytes == 0
            || limits.memory_bytes == u64::MAX
        {
            return Err(Error::new("compile_failed"));
        }
        let key = CacheKey::new(
            validated.artifact_digest,
            ENGINE_VERSION,
            std::env::consts::ARCH,
            ENGINE_CONFIG,
            INTERFACE_VERSION,
        )?;
        let child = Command::new(&self.child_path)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| Error::new("compile_failed"))?;
        self.last_child = Some(child);
        self.spawned.fetch_add(1, Ordering::Release);
        let child = self.last_child.as_mut().expect("just spawned child");
        // Before this returns, even setup and I/O failures terminate and reap.
        let result = run(child, validated.bytes(), limits, cancelled);
        let cleanup = terminate_and_reap(child);
        cleanup?;
        result?;
        Ok(key)
    }
}

fn apply_limits(child: &Child, limits: CompileLimits) -> Result<(), Error> {
    let pid = Some(Pid::from_child(child));
    for (resource, value) in [
        (Resource::Core, 0),
        (Resource::Cpu, limits.cpu_seconds),
        (Resource::As, limits.memory_bytes),
    ] {
        let limit = Rlimit {
            current: Some(value),
            maximum: Some(value),
        };
        prlimit(pid, resource, limit).map_err(|_| Error::new("compile_failed"))?;
        // Read back by applying the same limit; fail closed if it was not set.
        let applied = prlimit(pid, resource, limit).map_err(|_| Error::new("compile_failed"))?;
        if applied != limit {
            return Err(Error::new("compile_failed"));
        }
    }
    Ok(())
}

fn run(
    child: &mut Child,
    bytes: &[u8],
    limits: CompileLimits,
    cancelled: &AtomicBool,
) -> Result<(), Error> {
    apply_limits(child, limits)?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| Error::new("compile_failed"))?;
    thread::scope(|scope| {
        // A pipe can block. Keep wall/cancel enforcement active during transfer.
        let writer = thread::Builder::new()
            .spawn_scoped(scope, move || stdin.write_all(bytes))
            .map_err(|_| Error::new("compile_failed"))?;
        let result = monitor(child, limits.wall_clock_deadline, cancelled);
        // Kill before joining the writer so no pipe write can strand the caller.
        let cleanup = terminate_and_reap(child);
        let written = writer.join();
        cleanup?;
        result?;
        match written {
            Ok(Ok(())) => Ok(()),
            _ => Err(Error::new("compile_failed")),
        }
    })
}

fn monitor(child: &mut Child, deadline: Instant, cancelled: &AtomicBool) -> Result<(), Error> {
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Err(Error::new("compile_cancelled"));
        }
        if Instant::now() >= deadline {
            return Err(Error::new("compile_deadline_exceeded"));
        }
        if let Some(status) = child.try_wait().map_err(|_| Error::new("compile_failed"))? {
            return match (status.code(), status.signal()) {
                (Some(0), None) => Ok(()),
                (_, Some(24 | 9)) => Err(Error::new("compile_cpu_limit_exceeded")),
                (Some(66), None) | (_, Some(6)) => Err(Error::new("compile_memory_limit_exceeded")),
                _ => Err(Error::new("compile_failed")),
            };
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn terminate_and_reap(child: &mut Child) -> Result<(), Error> {
    // kill may report an already-exited child; wait is still mandatory.
    let _ = child.kill();
    child.wait().map_err(|_| Error::new("compile_failed"))?;
    Ok(())
}
