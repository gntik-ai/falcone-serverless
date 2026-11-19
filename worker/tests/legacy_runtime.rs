//! Operator-only execution of trusted-image-built components. Never builds a guest toolchain.
#[path = "support/legacy_context.rs"]
mod legacy_context;
#[path = "support/legacy_oracle.rs"]
mod oracle;

use falcone_serverless_worker::{
    adapters::javascript,
    api,
    artifact::{self, supervisor::CompileLimits},
    cache,
    host::{self, Bindings, Context, HostError, Operation, Reply},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[derive(Default)]
struct Platform {
    logs: Mutex<Vec<String>>,
    calls: AtomicUsize,
}
impl Bindings for Platform {
    fn call(&self, context: &Context, operation: Operation) -> Result<Reply, HostError> {
        assert_eq!(context.tenant, "tenant-synthetic-0001");
        assert_eq!(context.workspace, "ws-synthetic-0001");
        match operation {
            Operation::Log { message, .. } => {
                self.logs.lock().unwrap().push(message);
                Ok(Reply::Empty)
            }
            _ => {
                self.calls.fetch_add(1, Ordering::Relaxed);
                Err(HostError::Denied)
            }
        }
    }
}

#[test]
fn host_storage_scope_denials_reach_the_adapter_as_inventory_failures() {
    struct Storage(AtomicUsize);
    impl Bindings for Storage {
        fn call(&self, context: &Context, operation: Operation) -> Result<Reply, HostError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            assert_eq!(context.tenant, "tenant-synthetic-0001");
            assert_eq!(context.workspace, "ws-synthetic-0001");
            match operation {
                Operation::Read { key, .. } if key == "tenants/synthetic/good" => {
                    Ok(Reply::Data(vec![]))
                }
                _ => panic!("unauthorized input reached platform bindings"),
            }
        }
    }
    let directory = std::env::temp_dir().join(format!("legacy-host-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let scratch = Scratch(directory);
    let mut compiler =
        cache::Cache::new(&scratch.0, env!("CARGO_BIN_EXE_artifact-compile-child")).unwrap();
    let bytes = wat::parse_str(include_str!("fixtures/legacy/host-storage.wat")).unwrap();
    let loaded = compiler
        .compile(
            cache::CONTRACT_VERSION,
            &bytes,
            CompileLimits {
                input_bytes: 64 * 1024 * 1024,
                cpu_seconds: 120,
                memory_bytes: 4 * 1024 * 1024 * 1024,
                wall_clock_deadline: Instant::now() + Duration::from_secs(300),
            },
            &AtomicBool::new(false),
        )
        .unwrap();
    let expected: Value =
        serde_json::from_str(include_str!("fixtures/legacy/expected.json")).unwrap();
    for (key, allowed) in [
        ("tenants/synthetic/good", true),
        ("tenants/forged-tenant/object", false),
        ("workspaces/forged-workspace/object", false),
    ] {
        let mut wire: Value = serde_json::from_str(include_str!(
            "fixtures/contract/request/valid/wasm-no-base.json"
        ))
        .unwrap();
        wire["projection"]["artifact_digest"] =
            json!(format!("sha256:{:x}", Sha256::digest(&bytes)));
        wire["projection"]["imports"] = json!(["falcone:host/storage@0.2.0"]);
        wire["params"]["key"] = json!(key);
        let request = api::parse_request(&serde_json::to_vec(&wire).unwrap()).unwrap();
        let context = request.plan().context;
        let storage = Arc::new(Storage(AtomicUsize::new(0)));
        let (status, response) = javascript::qualify(
            &request,
            Some(&context),
            include_str!("fixtures/legacy/denied.js"),
            "{}",
            |input| {
                let params: Value = serde_json::from_slice(&input.params).unwrap();
                // This WAT probe drives the same WIT storage interface as denied.js.
                // It receives the key directly; it does not exercise JS packet decoding.
                let execution = host::execute(
                    &loaded,
                    host::Host::new(input.context, storage.clone()),
                    params["key"].as_str().unwrap().as_bytes(),
                );
                if !allowed {
                    // The guest returns this only after observing host-error.denied.
                    // Traps, instantiation failures and other host errors cannot pass.
                    assert_eq!(
                        execution.result.as_ref().unwrap_err().code,
                        "handler_failed"
                    );
                }
                javascript::Outcome::from_execution(execution, vec![])
            },
        )
        .unwrap();
        assert_eq!(storage.0.load(Ordering::Relaxed), usize::from(allowed));
        let response: Value = serde_json::from_slice(&response).unwrap();
        if allowed {
            assert_eq!(status, 200);
            assert_eq!(response["status"], "success");
            assert_eq!(response["result"], json!({"statusCode":200,"body":true}));
        } else {
            assert_eq!(status, 500);
            assert_eq!(response["status"], "failure");
            oracle::compare(
                &serde_json::to_vec(&response["result"]).unwrap(),
                &serde_json::to_vec(&expected["error"]).unwrap(),
                &[],
                &[],
            )
            .unwrap();
            assert!(!response.to_string().contains("forged-"));
        }
    }
}

#[test]
#[ignore = "UNVERIFIED: requires FS_TOOLCHAIN_CACHE and trusted-image-built JS components"]
fn built_legacy_components() {
    let Some(cache_root) = std::env::var_os("FS_TOOLCHAIN_CACHE") else {
        println!("UNVERIFIED: FS_TOOLCHAIN_CACHE unset; no guest toolchain or component executed");
        return;
    };
    let components = std::env::var_os("FS_LEGACY_COMPONENTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(cache_root).join("legacy"));
    let exe = std::env::current_exe().unwrap();
    let parent = exe.parent().unwrap();
    let binaries = if parent.file_name().is_some_and(|name| name == "deps") {
        parent.parent().unwrap()
    } else {
        parent
    };
    let directory = std::env::temp_dir().join(format!("legacy-components-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let scratch = Scratch(directory.clone());
    let mut compiler =
        cache::Cache::new(&directory, binaries.join("artifact-compile-child")).unwrap();
    let expected: Value =
        serde_json::from_str(include_str!("fixtures/legacy/expected.json")).unwrap();
    let variables: Vec<_> = expected["variables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|field| oracle::Variable {
            pointer: field["pointer"].as_str().unwrap(),
            schema: field["schema"].clone(),
        })
        .collect();
    let prefixes: Vec<_> = expected["logPrefixes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|prefix| match prefix["kind"].as_str().unwrap() {
            "timestamp" => oracle::Prefix::Timestamp,
            "stream" => oracle::Prefix::Stream(prefix["value"].as_str().unwrap()),
            _ => panic!("unrecognized inventory log prefix"),
        })
        .collect();
    for (name, source) in [
        ("success", include_str!("fixtures/legacy/success.js")),
        ("throw", include_str!("fixtures/legacy/throw.js")),
        ("reject", include_str!("fixtures/legacy/reject.js")),
        ("logs", include_str!("fixtures/legacy/logs.js")),
        ("identity", include_str!("fixtures/legacy/identity.js")),
        ("denied", include_str!("fixtures/legacy/denied.js")),
        ("guessed", include_str!("fixtures/legacy/guessed.js")),
    ] {
        let bytes = std::fs::read(components.join(format!("{name}.wasm")))
            .expect("missing trusted fixture component");
        // Validate before trusted compilation: even a WASI import left by the toolchain fails.
        let validated = artifact::validate::validate(&bytes, 64 * 1024 * 1024).unwrap();
        let mut wire: Value = serde_json::from_str(include_str!(
            "fixtures/contract/request/valid/wasm-no-base.json"
        ))
        .unwrap();
        wire["projection"]["artifact_digest"] =
            json!(format!("sha256:{:x}", Sha256::digest(&bytes)));
        wire["projection"]["imports"] = json!(validated.imports);
        wire["params"]["result"] = expected["success"].clone();
        wire["params"]["tenant"] = json!("forged-tenant");
        wire["params"]["workspace"] = json!("forged-workspace");
        wire["params"]["invocation_key"] = json!("forged-invocation");
        wire["params"]["__ow_headers"] =
            json!({"x-auth-tenant-id":"forged-tenant","x-workspace-id":"forged-workspace"});
        wire["params"]["key"] = json!("forged-tenant/forged-workspace/object");
        wire["params"]["handle"] = json!(42);
        let request = api::parse_request(&serde_json::to_vec(&wire).unwrap()).unwrap();
        let context = request.plan().context;
        let platform = Arc::new(Platform::default());
        let loaded = compiler
            .compile_declared(
                cache::CONTRACT_VERSION,
                &bytes,
                CompileLimits {
                    input_bytes: 64 * 1024 * 1024,
                    cpu_seconds: 120,
                    memory_bytes: 4 * 1024 * 1024 * 1024,
                    wall_clock_deadline: Instant::now() + Duration::from_secs(300),
                },
                &AtomicBool::new(false),
                Some(&request.plan().imports),
            )
            .unwrap();
        if name == "identity" {
            // Run the same pre-instantiation security cases inside the restricted pod as
            // offline. Reaching the executor boundary is a failure even if no handler runs.
            legacy_context::assert_rejected_contexts(&request, |pinned, legacy| {
                javascript::qualify(pinned, legacy, source, "{}", |_| {
                    panic!("invalid context reached instantiation")
                })
                .unwrap_err()
            });
        }
        let (status, response) =
            javascript::qualify(&request, Some(&context), source, "{}", |input| {
                let limits = request
                    .execution_limits(api::Ceilings {
                        max_memory_bytes: 64 * 1024 * 1024,
                        max_wall_clock_ms: 30_000,
                    })
                    .unwrap();
                let mut host =
                    host::Host::with_limits(input.context.clone(), platform.clone(), limits.limits);
                host.set_fuel(limits.fuel);
                host.set_max_host_calls(limits.max_host_calls);
                host.set_until(Instant::now() + limits.timeout);
                let execution = host::execute(&loaded, host, &input.wire().unwrap());
                javascript::Outcome::from_execution(
                    execution,
                    std::mem::take(&mut *platform.logs.lock().unwrap()),
                )
            })
            .unwrap();
        let response: Value = serde_json::from_slice(&response).unwrap();
        if matches!(name, "throw" | "reject" | "denied" | "guessed") {
            assert_eq!(status, 500, "{name}");
            assert_eq!(response["status"], "failure", "{name}");
            oracle::compare(
                &serde_json::to_vec(&response["result"]).unwrap(),
                &serde_json::to_vec(&expected["error"]).unwrap(),
                &[],
                &[],
            )
            .unwrap();
            // denied exercises the host storage-prefix gate. guessed may fail during
            // component-model resource lowering; it proves failure and no platform call,
            // not that a forged handle reached the host.
            if matches!(name, "denied" | "guessed") {
                assert_eq!(
                    platform.calls.load(Ordering::Relaxed),
                    0,
                    "rejected storage input must not reach platform bindings"
                );
            }
        } else {
            assert_eq!(status, 200, "{name}");
            assert_eq!(response["status"], "success", "{name}");
            let result = if name == "identity" {
                &expected["identity"]
            } else {
                &expected["success"]
            };
            oracle::compare(
                &serde_json::to_vec(&response["result"]).unwrap(),
                &serde_json::to_vec(result).unwrap(),
                &variables,
                &[],
            )
            .unwrap();
        }
        let logs = if name == "logs" {
            expected["logs"].clone()
        } else {
            json!([])
        };
        oracle::compare(
            &serde_json::to_vec(&json!({"logs":response["logs"]})).unwrap(),
            &serde_json::to_vec(&json!({"logs":logs})).unwrap(),
            &[],
            &prefixes,
        )
        .unwrap();
    }
    drop(compiler);
    drop(scratch);
    assert!(!directory.exists());
    println!("{}", json!({"check":"legacy_javascript","status":"passed"}));
}
