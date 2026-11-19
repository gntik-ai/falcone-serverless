use falcone_serverless_worker::{
    adapters::{compat, javascript},
    api,
    artifact::supervisor::CompileLimits,
    cache, host,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

pub struct Denied;
impl host::Bindings for Denied {
    fn call(&self, _: &host::Context, _: host::Operation) -> Result<host::Reply, host::HostError> {
        Err(host::HostError::Denied)
    }
}

struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Real validation, trusted compilation and execution. Guest source toolchains are never
/// invoked here. Only synthesized WIT components or operator-built raw components enter.
pub fn execute(
    request: &api::ExecuteRequest,
    bytes: &[u8],
    bindings: Arc<dyn host::Bindings>,
) -> (u16, serde_json::Value) {
    let verdict = compat::check("function main(params) { return params; }", b"{}");
    execute_checked(request, bytes, bindings, &verdict)
}

pub fn execute_checked(
    request: &api::ExecuteRequest,
    bytes: &[u8],
    bindings: Arc<dyn host::Bindings>,
    verdict: &compat::Verdict,
) -> (u16, serde_json::Value) {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let directory = Scratch(std::env::temp_dir().join(format!(
        "legacy-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )));
    std::fs::create_dir(&directory.0).unwrap();
    let exe = std::env::current_exe().unwrap();
    let parent = exe.parent().unwrap();
    let binaries = if parent.file_name().is_some_and(|n| n == "deps") {
        parent.parent().unwrap()
    } else {
        parent
    };
    let mut cache =
        cache::Cache::new(&directory.0, binaries.join("artifact-compile-child")).unwrap();
    let cancelled = Arc::new(AtomicBool::new(false));
    let loaded = cache
        .compile(
            cache::CONTRACT_VERSION,
            bytes,
            CompileLimits {
                input_bytes: 64 * 1024 * 1024,
                cpu_seconds: 120,
                memory_bytes: 4 * 1024 * 1024 * 1024,
                wall_clock_deadline: Instant::now() + Duration::from_secs(300),
            },
            &cancelled,
        )
        .unwrap();
    let plan = request.plan();
    let (status, body) =
        javascript::qualify(request, Some(&plan), verdict, || javascript::Runtime {
            loaded: &loaded,
            bindings,
            cancelled,
            limits: request
                .execution_limits(api::Ceilings {
                    max_memory_bytes: 64 * 1024 * 1024,
                    max_wall_clock_ms: 30_000,
                })
                .unwrap(),
            until: Instant::now() + Duration::from_secs(30),
        })
        .unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}
