//! Operator-only runner of prebuilt Go components. Never builds a guest or toolchain.
//! The cluster pod runs this binary; inputs and expectations are embedded, not read from
//! a checkout. The reused replay bindings are a platform boundary, not mocked Go code.
#[path = "support/profile_javascript.rs"]
pub mod replay;

use falcone_serverless_worker::{
    api,
    artifact::{self, supervisor::CompileLimits},
    cache, host,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

const MANIFEST: &str = include_str!("../qualification/profiles/go/manifest.toml");
const REQUEST: &str =
    include_str!("../qualification/profiles/go/examples/storage-events/request.json");
const CASES: &str = include_str!("../qualification/profiles/go/examples/storage-events/cases.json");

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn invoke(
    compiler: &mut cache::Cache,
    bytes: &[u8],
    imports: &[String],
    params: &Value,
    state: &BTreeMap<String, Vec<u8>>,
) -> (Vec<replay::Call>, Value) {
    let base: Value = serde_json::from_str(REQUEST).unwrap();
    let wire = replay::request_for(
        &base,
        &format!("sha256:{:x}", Sha256::digest(bytes)),
        imports,
        params,
    );
    let request = api::parse_request(&serde_json::to_vec(&wire).unwrap()).unwrap();
    let loaded = compiler
        .compile_declared(
            cache::CONTRACT_VERSION,
            bytes,
            CompileLimits {
                input_bytes: 64 * 1024 * 1024,
                cpu_seconds: 120,
                memory_bytes: 4 * 1024 * 1024 * 1024,
                wall_clock_deadline: Instant::now() + Duration::from_secs(300),
            },
            &AtomicBool::new(false),
            Some(imports),
        )
        .expect("trusted compilation failed");
    let plan = request.plan();
    let context = plan.context;
    let bindings = Arc::new(replay::Replay::new(
        state,
        &context.tenant,
        &context.workspace,
    ));
    let limits = request
        .execution_limits(api::Ceilings {
            max_memory_bytes: 64 * 1024 * 1024,
            max_wall_clock_ms: 30_000,
        })
        .unwrap();
    let mut host = host::Host::with_limits(context, bindings.clone(), limits.limits);
    host.set_fuel(limits.fuel);
    host.set_max_host_calls(limits.max_host_calls);
    host.set_until(Instant::now() + limits.timeout);
    let result = host::execute(&loaded, host, &plan.params)
        .result
        .expect("Go handler failed");
    (
        bindings.calls(),
        serde_json::from_slice(&result).expect("Go handler returned non-JSON"),
    )
}

#[test]
#[ignore = "UNVERIFIED: requires FS_TOOLCHAIN_CACHE and trusted-image-built Go components"]
fn built_profile_components() {
    let Some(cache_root) = std::env::var_os("FS_TOOLCHAIN_CACHE") else {
        println!("UNVERIFIED: FS_TOOLCHAIN_CACHE unset; no Go component executed");
        return;
    };
    let components = std::env::var_os("FS_GO_COMPONENTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(cache_root).join("go-profile"));
    let manifest: toml::Value = toml::from_str(MANIFEST).unwrap();
    // Check the entire set before compiling or instantiating anything. Leftover WASI,
    // extra exports, or any interface outside the declared per-example set fail closed.
    let mut checked = BTreeMap::new();
    for workload in manifest["workloads"].as_array().unwrap() {
        let name = workload["name"].as_str().unwrap();
        let bytes = std::fs::read(components.join(format!("{name}.wasm")))
            .expect("missing trusted-image-built Go component");
        let validated = artifact::validate::validate(&bytes, 64 * 1024 * 1024)
            .expect("Go component rejected by the worker import/export gate");
        let declared: BTreeSet<_> = workload["imports"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i.as_str().unwrap())
            .collect();
        assert_eq!(
            validated
                .imports
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            declared
        );
        println!(
            "{}",
            json!({"component": name, "sha256": format!("{:x}", Sha256::digest(&bytes))})
        );
        let imports = validated.imports;
        checked.insert(name, (bytes, imports));
    }
    let exe = std::env::current_exe().unwrap();
    let parent = exe.parent().unwrap();
    let binaries = if parent.file_name().is_some_and(|name| name == "deps") {
        parent.parent().unwrap()
    } else {
        parent
    };
    let directory = std::env::temp_dir().join(format!("profile-go-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let scratch = Scratch(directory.clone());
    let mut compiler =
        cache::Cache::new(&directory, binaries.join("artifact-compile-child")).unwrap();
    for (name, input, expected) in [
        (
            "json-transform",
            include_str!("../qualification/profiles/go/examples/json-transform/input.json"),
            include_str!("../qualification/profiles/go/examples/json-transform/expected.json"),
        ),
        (
            "hash-sha256",
            include_str!("../qualification/profiles/go/examples/hash-sha256/input.json"),
            include_str!("../qualification/profiles/go/examples/hash-sha256/expected.json"),
        ),
    ] {
        let (bytes, imports) = &checked[name];
        let mut params: Value = serde_json::from_str(input).unwrap();
        params["__ow_path"] = json!("/v1/workspaces/ws-synthetic-0001/openapi");
        let (calls, actual) = invoke(&mut compiler, bytes, imports, &params, &BTreeMap::new());
        assert!(calls.is_empty(), "pure workload made a platform call");
        assert_eq!(actual, serde_json::from_str::<Value>(expected).unwrap());
    }
    let (bytes, imports) = &checked["storage-events"];
    for case in replay::load_cases(CASES) {
        let (calls, actual) = invoke(&mut compiler, bytes, imports, &case.params, &case.state);
        replay::compare(&case, &calls, &actual).unwrap();
    }
    drop(compiler);
    drop(scratch);
    // Only this fixed record is consumed by the cluster harness. No guest output or
    // credential material is logged. The operator still must publish evidence by ID/hash.
    println!("{}", json!({"check":"profile_go","status":"passed"}));
}
