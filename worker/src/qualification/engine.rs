//! Wasmtime adapter for the two embedded, import-free qualification fixtures.

use crate::{runner::Mode, settings};
use wasmtime::{Config, Engine, Store, Strategy};

pub fn build_engine() -> Result<Engine, String> {
    let mut config = Config::new();
    config
        .strategy(Strategy::Cranelift)
        .consume_fuel(settings::CONSUME_FUEL)
        .epoch_interruption(settings::EPOCH_INTERRUPTION);
    Engine::new(&config).map_err(|error| error.to_string())
}

pub fn build_store(engine: &Engine, mode: Mode) -> Result<Store<()>, String> {
    let mut store = Store::new(engine, ());
    store
        .set_fuel(if mode == Mode::EpochOnly {
            settings::EPOCH_RUN_FUEL
        } else {
            settings::FUEL_LIMIT
        })
        .map_err(|error| error.to_string())?;
    // FuelOnly must use an engine whose epoch is still zero: Wasmtime adds the
    // delta to the current epoch. The real harness keeps a separate fuel engine.
    store.set_epoch_deadline(if mode == Mode::FuelOnly {
        settings::FUEL_RUN_EPOCH_DEADLINE
    } else {
        settings::EPOCH_DEADLINE_TICKS
    });
    store.epoch_deadline_trap();
    Ok(store)
}

const VALID: &str = include_str!("fixtures/valid.wat");
const INFINITE_LOOP: &str = include_str!("fixtures/infinite_loop.wat");

use crate::{
    outcome::{self, Outcome, RunError, RunResult},
    runner::Harness,
};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    sync::mpsc,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use wasmtime::{Instance, Module, Trap};

struct Fixtures {
    valid: Module,
    epoch: Module,
    fuel: Module,
}

pub struct RealHarness {
    // The fuel engine is never ticked. Its maximum deadline is therefore valid
    // even after Standard and EpochOnly have advanced the other engine's epoch.
    engines: Result<(Engine, Engine), String>,
    fixtures: Option<Fixtures>,
    ticker: Option<Ticker>,
}

impl RealHarness {
    pub fn new() -> Self {
        Self {
            engines: build_engine().and_then(|epoch| Ok((epoch, build_engine()?))),
            fixtures: None,
            ticker: None,
        }
    }

    fn module(&self, mode: Mode) -> Result<&Module, String> {
        let fixtures = self.fixtures.as_ref().ok_or("fixtures not compiled")?;
        Ok(match mode {
            Mode::Standard => &fixtures.valid,
            Mode::EpochOnly => &fixtures.epoch,
            Mode::FuelOnly => &fixtures.fuel,
        })
    }
}

impl Harness for RealHarness {
    fn engine_config(&mut self) -> Result<(), String> {
        self.engines.as_ref().map(|_| ()).map_err(Clone::clone)
    }

    fn compile_valid(&mut self) -> Result<(), String> {
        let (epoch, fuel) = self.engines.as_ref().map_err(Clone::clone)?;
        let compile = |engine: &Engine, source: &str| -> Result<Module, String> {
            let module = Module::new(engine, source).map_err(|error| error.to_string())?;
            if module.imports().len() != 0 {
                return Err("embedded fixture unexpectedly has imports".into());
            }
            Ok(module)
        };
        self.fixtures = Some(Fixtures {
            valid: compile(epoch, VALID)?,
            epoch: compile(epoch, INFINITE_LOOP)?,
            fuel: compile(fuel, INFINITE_LOOP)?,
        });
        Ok(())
    }

    fn load_executable_memory(&mut self) -> Result<(), String> {
        for mode in [Mode::Standard, Mode::EpochOnly, Mode::FuelOnly] {
            let module = self.module(mode)?;
            let mut store = build_store(module.engine(), mode)?;
            Instance::new(&mut store, module, &[]).map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn start_ticker(&mut self, tick_ms: u64) -> Result<(), String> {
        if self.ticker.is_some() {
            return Err("epoch ticker already running".into());
        }
        let engine = self.engines.as_ref().map_err(Clone::clone)?.0.clone();
        self.ticker = Some(Ticker::start(engine, tick_ms)?);
        Ok(())
    }

    fn stop_ticker(&mut self) -> Result<(), String> {
        self.ticker.take().ok_or("epoch ticker not running")?.stop()
    }

    fn run(&mut self, mode: Mode) -> RunResult {
        let module = self.module(mode).map_err(RunError::Other)?;
        let mut store = build_store(module.engine(), mode).map_err(RunError::Other)?;
        let instance = Instance::new(&mut store, module, &[]).map_err(run_error)?;
        match mode {
            Mode::Standard => instance
                .get_typed_func::<(), i32>(&mut store, "run")
                .map_err(run_error)?
                .call(&mut store, ())
                .map_err(run_error),
            Mode::EpochOnly | Mode::FuelOnly => instance
                .get_typed_func::<(), ()>(&mut store, "run")
                .map_err(run_error)?
                .call(&mut store, ())
                .map(|()| 0)
                .map_err(run_error),
        }
    }

    fn root_fs_read_only(&mut self) -> Outcome {
        let mountinfo = fs::read_to_string("/proc/self/mountinfo").ok();
        outcome::root_fs_read_only(
            mountinfo.as_deref(),
            write_probe(Path::new("/was01-root-write-probe")),
        )
    }

    fn tmp_writable(&mut self) -> Outcome {
        let stamp = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(stamp) => stamp.as_nanos(),
            Err(error) => return Outcome::failed(error.to_string(), "posture"),
        };
        let path = format!("/tmp/was01-write-probe-{}-{stamp}", std::process::id());
        let result = (|| {
            let mut payload = [0; 32];
            fs::File::open("/dev/urandom")
                .and_then(|mut random| random.read_exact(&mut payload))
                .map_err(|error| error.to_string())?;
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|error| error.to_string())?;
            let result = tmp_round_trip(&mut file, &payload);
            drop(file);
            let cleanup = fs::remove_file(&path).map_err(|error| error.to_string());
            result.and(cleanup)
        })();
        outcome::tmp_writable(result)
    }

    fn non_root_uid(&mut self) -> Outcome {
        let status = fs::read_to_string("/proc/self/status").ok();
        outcome::non_root_uid(status.as_deref())
    }
}

/// Filesystem I/O boundary for the /tmp probe; success requires a byte-for-byte readback.
pub fn tmp_round_trip(file: &mut (impl Read + Write + Seek), payload: &[u8]) -> Result<(), String> {
    file.write_all(payload)
        .and_then(|()| file.flush())
        .and_then(|()| file.seek(SeekFrom::Start(0)).map(|_| ()))
        .map_err(|error| error.to_string())?;
    let mut readback = vec![0; payload.len()];
    file.read_exact(&mut readback)
        .map_err(|error| error.to_string())?;
    if readback != payload {
        return Err("/tmp payload readback differs".into());
    }
    Ok(())
}

fn run_error(error: wasmtime::Error) -> RunError {
    match error.downcast_ref::<Trap>() {
        Some(Trap::Interrupt) => RunError::Interrupt,
        Some(Trap::OutOfFuel) => RunError::OutOfFuel,
        _ => RunError::Other(error.to_string()),
    }
}

fn write_probe(path: &Path) -> Result<(), String> {
    // Never overwrite or remove a preexisting file, even on an unqualified host.
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    let write = file.write_all(b"WAS-01\n").and_then(|()| file.flush());
    drop(file);
    // Cleanup cannot turn a successful root write into read-only evidence.
    let _ = fs::remove_file(path);
    write.map_err(|error| error.to_string())
}

#[cfg(test)]
mod tmp_tests {
    use super::*;
    use std::io::{self, Cursor};

    // Mock only the filesystem I/O boundary, including faults ordinary temp files cannot produce.
    struct ProbeFile {
        bytes: Cursor<Vec<u8>>,
        fault: &'static str,
    }

    impl Read for ProbeFile {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            if self.fault == "read" {
                return Err(io::Error::other("read denied"));
            }
            let count = self.bytes.read(bytes)?;
            if self.fault == "mismatch" {
                bytes[..count].fill(0);
            }
            Ok(count)
        }
    }
    impl Write for ProbeFile {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.fault == "write" {
                return Err(io::Error::other("write denied"));
            }
            self.bytes.write(bytes)
        }
        fn flush(&mut self) -> io::Result<()> {
            if self.fault == "flush" {
                return Err(io::Error::other("flush denied"));
            }
            Ok(())
        }
    }
    impl Seek for ProbeFile {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            self.bytes.seek(position)
        }
    }

    #[test]
    fn tmp_round_trip_success_and_read_write_comparison_failures() {
        for (fault, expected) in [
            ("", None),
            ("read", Some("read denied")),
            ("write", Some("write denied")),
            ("flush", Some("flush denied")),
            ("mismatch", Some("/tmp payload readback differs")),
        ] {
            let mut file = ProbeFile {
                bytes: Cursor::new(Vec::new()),
                fault,
            };
            let result = tmp_round_trip(&mut file, b"independent fixture payload");
            assert_eq!(result.as_ref().err().map(String::as_str), expected);
            let classified = outcome::tmp_writable(result);
            if expected.is_some() {
                assert_eq!(classified.status, outcome::Status::Failed);
                assert_eq!(classified.error_class, Some("posture"));
            } else {
                assert_eq!(
                    serde_json::to_value(classified).unwrap()["status"],
                    "passed"
                );
                assert_eq!(file.bytes.into_inner(), b"independent fixture payload");
            }
        }
    }

    #[test]
    fn tmp_probe_reports_verified_readback() {
        let result = RealHarness::new().tmp_writable();
        assert_eq!(serde_json::to_value(&result).unwrap()["status"], "passed");
        assert_eq!(result.reason, "/tmp random payload readback verified");
    }
}

struct Ticker {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Ticker {
    fn start(engine: Engine, tick_ms: u64) -> Result<Self, String> {
        let (stop, receiver) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("was01-epoch".into())
            .spawn(move || {
                while let Err(mpsc::RecvTimeoutError::Timeout) =
                    receiver.recv_timeout(Duration::from_millis(tick_ms))
                {
                    engine.increment_epoch();
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(Self {
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    fn stop(&mut self) -> Result<(), String> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| "epoch ticker panicked".to_owned())?;
        }
        Ok(())
    }
}

impl Drop for Ticker {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::Mode;

    #[test]
    fn engine_builds_and_stores_have_the_required_fuel_without_instantiation() {
        let engine = build_engine().expect("pinned Cranelift configuration");
        for (mode, expected) in [
            (Mode::Standard, 10_000_000),
            (Mode::FuelOnly, 10_000_000),
            (Mode::EpochOnly, u64::MAX),
        ] {
            let store = build_store(&engine, mode).unwrap();
            assert_eq!(store.get_fuel().unwrap(), expected, "{mode:?}");
        }
    }
}
