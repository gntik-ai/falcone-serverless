//! Restricted-pod entry points of the execution API qualification (`--execute-api=<name>`).
//!
//! Most names run one case of `execution_api` in this pod, against servers it starts itself, with
//! the documented production deadlines. The production-image pod has three containers in one pod
//! network and one scratch volume: `production-prepare` (init) writes the worker configuration
//! and the artifact, the production worker image serves them as a sidecar, and
//! `production-client` drives its two listeners over loopback.

use super::execution_api as cases;
use serde_json::json;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::Ordering;

/// Where the pod's containers share their scratch volume.
const SHARED: &str = "/shared";
/// Execute, control, metrics and probe ports of the production worker in the pod.
const PORTS: [u16; 4] = [18080, 18081, 18082, 18083];

fn production_prepare() -> Result<(), String> {
    let root = Path::new(SHARED);
    let artifacts = root.join("artifacts");
    let cache = root.join("cache");
    for directory in [&artifacts, &cache] {
        std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    }
    let bytes = cases::production_artifact();
    let digest = cases::production_digest();
    std::fs::write(artifacts.join(format!("{digest}.wasm")), bytes)
        .map_err(|error| error.to_string())?;
    let [execute, control, metrics, probe] = PORTS;
    std::fs::write(
        root.join("worker.toml"),
        cases::worker_config(execute, control, metrics, probe, root),
    )
    .map_err(|error| error.to_string())
}

fn production_client() {
    let at = |port| SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    cases::the_production_worker_serves_the_execution_api(at(PORTS[0]), at(PORTS[1]));
}

/// Runs one case, or reports it unknown.
pub fn run(name: &str) -> ExitCode {
    cases::PRODUCTION_TIMING.store(true, Ordering::Release);
    match name {
        "production-prepare" => {
            if let Err(error) = production_prepare() {
                eprintln!("production_prepare_failed: {error}");
                return ExitCode::FAILURE;
            }
        }
        "production-client" => production_client(),
        _ => {
            let Some((_, case)) = cases::CASES
                .iter()
                .find(|(candidate, _)| *candidate == name)
            else {
                eprintln!("unknown_execute_api_case");
                return ExitCode::FAILURE;
            };
            case();
        }
    }
    println!("{}", json!({"case": name, "status": "passed"}));
    println!("{}", json!({"check": "execute_api", "status": "passed"}));
    ExitCode::SUCCESS
}
