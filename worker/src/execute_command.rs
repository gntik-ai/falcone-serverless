//! `execute`: one `handle` call on a component, through the trusted compile path.
//!
//! The raw component passes the artifact validator, then the supervised compiler child
//! and the pod-local cache; only that output runs. The result goes to stdout only on
//! success. A failure writes one bounded code and no output. No platform bindings exist
//! yet, so every capability call the guest makes is denied.

use falcone_serverless_worker::artifact::supervisor::CompileLimits;
use falcone_serverless_worker::cache::{CONTRACT_VERSION, Cache};
use falcone_serverless_worker::host::execute::{self, Executor, Limits};
use falcone_serverless_worker::host::{
    BUFFER_BYTES, Bindings, Context, Host, HostError, Operation, Reply,
};
use std::ffi::OsString;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

const USAGE: &str = "usage: falcone-serverless-worker execute --component PATH --params PATH \
--cache-dir DIR [--deadline-ms N] [--compiler PATH]";
const DEFAULT_DEADLINE: Duration = Duration::from_secs(30);
const COMPONENT_BYTES: usize = 64 * 1024 * 1024;

struct Denied;

impl Bindings for Denied {
    fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
        Err(HostError::Denied)
    }
}

struct Arguments {
    component: PathBuf,
    params: PathBuf,
    cache_dir: PathBuf,
    deadline: Duration,
    compiler: Option<PathBuf>,
}

fn parse(args: &[OsString]) -> Result<Arguments, String> {
    let (mut component, mut params, mut cache_dir, mut compiler) = (None, None, None, None);
    let mut deadline = DEFAULT_DEADLINE;
    let mut args = args.iter();
    while let Some(flag) = args.next() {
        let value = args.next().ok_or(USAGE)?;
        match flag.to_str() {
            Some("--component") => component = Some(PathBuf::from(value)),
            Some("--params") => params = Some(PathBuf::from(value)),
            Some("--cache-dir") => cache_dir = Some(PathBuf::from(value)),
            Some("--compiler") => compiler = Some(PathBuf::from(value)),
            Some("--deadline-ms") => {
                let millis = value
                    .to_str()
                    .and_then(|text| text.parse::<u64>().ok())
                    .filter(|millis| *millis > 0)
                    .ok_or(USAGE)?;
                deadline = Duration::from_millis(millis);
            }
            _ => return Err(USAGE.into()),
        }
    }
    Ok(Arguments {
        component: component.ok_or(USAGE)?,
        params: params.ok_or(USAGE)?,
        cache_dir: cache_dir.ok_or(USAGE)?,
        deadline,
        compiler,
    })
}

/// Reads at most `limit` bytes of a regular file; `None` when the file is larger.
fn read_bounded(path: &PathBuf, limit: usize) -> Result<Option<Vec<u8>>, String> {
    let file = std::fs::File::open(path).map_err(|_| "unreadable_input".to_owned())?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "unreadable_input".to_owned())?;
    Ok((bytes.len() <= limit).then_some(bytes))
}

pub fn run(args: &[OsString], stdout: &mut dyn std::io::Write) -> Result<(), String> {
    let arguments = parse(args)?;
    let params = read_bounded(&arguments.params, BUFFER_BYTES)?
        .ok_or_else(|| execute::LIMIT_EXCEEDED.to_owned())?;
    let component = read_bounded(&arguments.component, COMPONENT_BYTES)?
        .ok_or_else(|| "compile_size_limit_exceeded".to_owned())?;
    let compiler = match arguments.compiler {
        Some(path) => path,
        None => std::env::current_exe()
            .map_err(|_| "executable_path".to_owned())?
            .with_file_name("artifact-compile-child"),
    };
    let mut cache =
        Cache::new(arguments.cache_dir, compiler).map_err(|error| error.code.to_owned())?;
    let limits = CompileLimits {
        input_bytes: COMPONENT_BYTES,
        cpu_seconds: 120,
        memory_bytes: 4 * 1024 * 1024 * 1024,
        wall_clock_deadline: Instant::now() + Duration::from_secs(300),
    };
    let compiled = cache
        .compile(
            CONTRACT_VERSION,
            &component,
            limits,
            &AtomicBool::new(false),
        )
        .map_err(|error| error.code.to_owned())?;
    let executor =
        Executor::new(compiled.component.engine()).map_err(|error| error.code.to_owned())?;
    let host = Host::new(Context::default(), Arc::new(Denied));
    let result = executor
        .execute(
            &compiled.component,
            host,
            &params,
            Limits {
                guest_deadline: arguments.deadline,
            },
        )
        .map_err(|error| error.code.to_owned())?;
    stdout
        .write_all(&result)
        .and_then(|()| stdout.flush())
        .map_err(|_| "output_unwritable".to_owned())
}
