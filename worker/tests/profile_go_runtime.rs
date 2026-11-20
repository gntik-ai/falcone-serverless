//! Operator-only execution of the trusted-image-built Go profile components. It never builds a
//! guest toolchain, and it is the binary the cluster suite runs in a restricted pod. Without
//! the opt-in it reports UNVERIFIED and runs nothing. The always-run tests cover the checks
//! that do not need a built component: the core-module shape rules and the memory limit.
use falcone_serverless_worker::{
    adapters::compat::TreeFile,
    adapters::compat_go::check_tree,
    api,
    artifact::{self, supervisor::CompileLimits},
    cache,
    host::{self, Bindings, Context, HostError, Operation, Reply},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::{Duration, Instant},
};
use wasmparser::{ExportSectionReader, Parser, Payload};

const MANIFEST: &str = include_str!("../qualification/profiles/go/manifest.toml");
const GO_MOD: &str = include_str!("../qualification/profiles/go/go.mod");
const EVENTS_REQUEST: &str =
    include_str!("../qualification/profiles/go/examples/events-handle/request.json");
const EVENTS_CASES: &str =
    include_str!("../qualification/profiles/go/examples/events-handle/cases.json");

/// What the import gate and the store limits require of a composed component's core modules.
#[derive(Debug, PartialEq, Eq)]
struct Shape {
    core_modules: usize,
    /// Core modules exporting `cabi_realloc`: the pinned TinyGo runtime exports exactly one.
    realloc_exporters: usize,
    /// Memories defined by core modules: the host's `memories(1)` limit allows exactly one.
    memories: usize,
}

fn core_shape(bytes: &[u8]) -> Result<Shape, String> {
    let mut shape = Shape {
        core_modules: 0,
        realloc_exporters: 0,
        memories: 0,
    };
    // Nested payloads follow their section; only core modules (not components) count.
    let mut kinds: Vec<bool> = Vec::new();
    let exports = |section: ExportSectionReader<'_>| -> Result<usize, String> {
        let mut found = 0;
        for export in section {
            if export.map_err(|e| e.to_string())?.name == "cabi_realloc" {
                found += 1;
            }
        }
        Ok(found)
    };
    for payload in Parser::new(0).parse_all(bytes) {
        match payload.map_err(|e| e.to_string())? {
            Payload::ModuleSection { .. } => {
                kinds.push(true);
                shape.core_modules += 1;
            }
            Payload::ComponentSection { .. } => kinds.push(false),
            Payload::End(_) => {
                kinds.pop();
            }
            Payload::MemorySection(section) if kinds.last() == Some(&true) => {
                shape.memories += section.count() as usize;
            }
            Payload::ExportSection(section) if kinds.last() == Some(&true) => {
                match exports(section)? {
                    0 => {}
                    1 => shape.realloc_exporters += 1,
                    _ => return Err("a core module exports cabi_realloc twice".into()),
                }
            }
            _ => {}
        }
    }
    Ok(shape)
}

fn one_module(extra: &str) -> String {
    format!(
        "(component (core module $m (memory (export \"memory\") 1) \
         (func (export \"cabi_realloc\") (param i32 i32 i32 i32) (result i32) i32.const 0) {extra}))"
    )
}

#[test]
fn a_component_with_one_realloc_and_one_memory_has_the_required_shape() {
    let bytes = wat::parse_str(one_module("")).unwrap();
    assert_eq!(
        core_shape(&bytes).unwrap(),
        Shape {
            core_modules: 1,
            realloc_exporters: 1,
            memories: 1
        }
    );
}

#[test]
fn a_second_realloc_exporter_or_a_second_memory_is_visible_in_the_shape() {
    // The wasi-virt composition added a second memory; a second runtime would add a second
    // cabi_realloc. Both must change the shape so the build check can refuse them.
    let two_memories = wat::parse_str(
        "(component (core module $a (memory 1) (func (export \"cabi_realloc\") (param i32 i32 i32 i32) (result i32) i32.const 0)) \
         (core module $b (memory 1)))",
    )
    .unwrap();
    assert_eq!(core_shape(&two_memories).unwrap().memories, 2);
    let two_reallocs = wat::parse_str(
        "(component (core module $a (memory 1) (func (export \"cabi_realloc\") (param i32 i32 i32 i32) (result i32) i32.const 0)) \
         (core module $b (func (export \"cabi_realloc\") (param i32 i32 i32 i32) (result i32) i32.const 0)))",
    )
    .unwrap();
    assert_eq!(core_shape(&two_reallocs).unwrap().realloc_exporters, 2);
    let none = wat::parse_str("(component (core module $a (memory 1)))").unwrap();
    assert_eq!(core_shape(&none).unwrap().realloc_exporters, 0);
    // A duplicate export name inside one module is not valid Wasm at all, so "two cabi_realloc
    // exports" can only mean two modules, which the shape reports above.
    let duplicate = wat::parse_str(
        "(module (memory 1) (func (export \"cabi_realloc\")) (func (export \"cabi_realloc\")))",
    )
    .unwrap();
    assert!(
        wasmparser::Validator::new()
            .validate_all(&duplicate)
            .is_err()
    );
}

struct Nothing;
impl Bindings for Nothing {
    fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
        Ok(Reply::Empty)
    }
}

#[test]
fn the_host_memory_limit_admits_one_linear_memory_and_refuses_a_second() {
    let engine = wasmtime::Engine::default();
    let start = |source: &str| {
        host::instantiate(
            &engine,
            source,
            host::Host::new(Context::default(), Arc::new(Nothing)),
        )
    };
    assert!(start(&one_module("")).is_ok());
    let two = "(component (core module $a (memory 1)) (core module $b (memory 1)) \
               (core instance (instantiate $a)) (core instance (instantiate $b)))";
    let error = start(two).map(|_| ()).unwrap_err();
    assert!(
        format!("{error:#}").to_lowercase().contains("memor"),
        "{error:#}"
    );
}

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Publishes recorded exactly; anything else a guest asks for is denied.
#[derive(Default)]
struct Recorder(Mutex<Vec<(String, Vec<u8>)>>);
impl Bindings for Recorder {
    fn call(&self, _: &Context, operation: Operation) -> Result<Reply, HostError> {
        match operation {
            Operation::Publish {
                destination,
                payload,
            } => {
                self.0.lock().unwrap().push((destination, payload));
                Ok(Reply::Empty)
            }
            Operation::Log { .. } => Ok(Reply::Empty),
            _ => Err(HostError::Denied),
        }
    }
}

fn compile(
    compiler: &mut cache::Cache,
    bytes: &[u8],
    request: &api::ExecuteRequest,
) -> cache::Loaded {
    compiler
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
            Some(&request.plan().imports),
        )
        .unwrap()
}

/// One invocation of the raw params under the store's real limits, including `memories(1)`.
fn invoke(
    loaded: &cache::Loaded,
    request: &api::ExecuteRequest,
    bindings: Arc<dyn Bindings>,
    params: &[u8],
) -> Result<Vec<u8>, (String, Option<String>)> {
    let limits = request
        .execution_limits(api::Ceilings {
            max_memory_bytes: 64 * 1024 * 1024,
            max_wall_clock_ms: 30_000,
        })
        .unwrap();
    let mut host = host::Host::with_limits(request.plan().context, bindings, limits.limits);
    host.set_fuel(limits.fuel);
    host.set_max_host_calls(limits.max_host_calls);
    host.set_until(Instant::now() + limits.timeout);
    host::execute(loaded, host, params)
        .result
        .map_err(|error| (error.code.to_owned(), error.message))
}

/// The request for one built component: the example request with the component's real imports
/// and artifact digest. Grants stay exactly as recorded.
fn request_for(digest: &str, imports: &[String]) -> api::ExecuteRequest {
    let mut wire: Value = serde_json::from_str(EVENTS_REQUEST).unwrap();
    wire["projection"]["artifact_digest"] = Value::String(digest.to_owned());
    wire["projection"]["imports"] = Value::from(imports.to_vec());
    api::parse_request(&serde_json::to_vec(&wire).unwrap())
        .expect("the grants must admit the component's imports")
}

fn digest_of(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

/// The import gate, the allow-list and the core-module shape rules for one built component.
fn admit(name: &str, bytes: &[u8], allowed: &BTreeSet<String>) -> Result<Vec<String>, String> {
    let validated = artifact::validate::validate(bytes, 64 * 1024 * 1024)
        .map_err(|e| format!("{name}: import gate rejected the component: {}", e.code))?;
    if let Some(import) = validated.imports.iter().find(|i| !allowed.contains(*i)) {
        return Err(format!("{name}: import outside the allowed list: {import}"));
    }
    let shape = core_shape(bytes)?;
    if shape.realloc_exporters != 1 {
        return Err(format!(
            "{name}: {} core modules export cabi_realloc, expected exactly one",
            shape.realloc_exporters
        ));
    }
    if shape.memories != 1 {
        return Err(format!(
            "{name}: {} linear memories, expected exactly one",
            shape.memories
        ));
    }
    Ok(validated.imports)
}

#[test]
#[ignore = "UNVERIFIED: requires FS_TOOLCHAIN_CACHE and trusted-image-built Go components"]
fn built_profile_components() {
    let Some(cache_root) = std::env::var_os("FS_TOOLCHAIN_CACHE") else {
        println!("UNVERIFIED: FS_TOOLCHAIN_CACHE unset; no guest toolchain or component executed");
        return;
    };
    let components = std::env::var_os("FS_PROFILE_COMPONENTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(cache_root).join("profile"));
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
    let manifest: toml::Value = toml::from_str(MANIFEST).unwrap();
    let mut allowed: BTreeSet<String> = BTreeSet::new();
    for table in ["wit", "wasi"] {
        for entry in manifest[table]["interfaces"].as_array().unwrap() {
            allowed.insert(entry.as_str().unwrap().to_owned());
        }
    }
    let mut failures: Vec<String> = Vec::new();
    let mut record = |name: &str, outcome: Result<(), String>| {
        println!("{name}: {outcome:?}");
        if let Err(reason) = outcome {
            failures.push(format!("{name}: {reason}"));
        }
    };

    // No negative fixture ever runs: each is only read as source and must keep its diagnostic.
    for fixture in manifest["negative"].as_array().unwrap() {
        let file = fixture["file"].as_str().unwrap();
        let source = match file {
            "negative/native-library/main.go" => {
                include_str!("../qualification/profiles/go/negative/native-library/main.go")
            }
            "negative/gpu/main.go" => {
                include_str!("../qualification/profiles/go/negative/gpu/main.go")
            }
            "negative/process-spawn/main.go" => {
                include_str!("../qualification/profiles/go/negative/process-spawn/main.go")
            }
            "negative/install-hook/main.go" => {
                include_str!("../qualification/profiles/go/negative/install-hook/main.go")
            }
            "negative/package-not-in-catalogue/main.go" => include_str!(
                "../qualification/profiles/go/negative/package-not-in-catalogue/main.go"
            ),
            "negative/unsupported-api/main.go" => {
                include_str!("../qualification/profiles/go/negative/unsupported-api/main.go")
            }
            other => panic!("unlisted negative fixture {other}"),
        };
        let files = [
            TreeFile {
                path: "go.mod",
                text: GO_MOD,
            },
            TreeFile {
                path: file,
                text: source,
            },
        ];
        let verdict = check_tree(&files);
        let code = fixture["code"].as_str().unwrap();
        let outcome = match verdict.diagnostics() {
            [one] if one.code == code && one.path == file => Ok(()),
            other => Err(format!("expected exactly {code}, got {other:?}")),
        };
        record(&format!("negative/{code}"), outcome);
    }

    // The pure workloads against their expected outputs, under the real limits.
    for (name, input, expected) in [
        (
            "hash-sha256",
            include_str!("../qualification/profiles/go/examples/hash-sha256/input.json"),
            include_str!("../qualification/profiles/go/examples/hash-sha256/expected.json"),
        ),
        (
            "byte-stats",
            include_str!("../qualification/profiles/go/examples/byte-stats/input.json"),
            include_str!("../qualification/profiles/go/examples/byte-stats/expected.json"),
        ),
        (
            "random-bytes",
            include_str!("../qualification/profiles/go/examples/random-bytes/input.json"),
            include_str!("../qualification/profiles/go/examples/random-bytes/expected.json"),
        ),
    ] {
        let outcome = (|| {
            let bytes = std::fs::read(components.join(format!("{name}.wasm")))
                .map_err(|_| "missing trusted component".to_owned())?;
            let imports = admit(name, &bytes, &allowed)?;
            let request = request_for(&digest_of(&bytes), &imports);
            let loaded = compile(&mut compiler, &bytes, &request);
            let output = invoke(&loaded, &request, Arc::new(Nothing), input.as_bytes())
                .map_err(|(code, message)| format!("handler failed: {code} {message:?}"))?;
            let actual: Value = serde_json::from_slice(&output)
                .map_err(|_| "handler result is not JSON".to_owned())?;
            let expected: Value = serde_json::from_str(expected).unwrap();
            (actual == expected)
                .then_some(())
                .ok_or_else(|| "output differs from the recorded expectation".to_owned())
        })();
        record(name, outcome);
    }

    // The events example: exact dependency calls and response for every recorded case.
    let events = (|| -> Result<(), String> {
        let bytes = std::fs::read(components.join("events-handle.wasm"))
            .map_err(|_| "missing trusted component".to_owned())?;
        let imports = admit("events-handle", &bytes, &allowed)?;
        let request = request_for(&digest_of(&bytes), &imports);
        let loaded = compile(&mut compiler, &bytes, &request);
        let cases: Value = serde_json::from_str(EVENTS_CASES).unwrap();
        for case in cases["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let params = case["params"].as_str().unwrap();
            let recorder = Arc::new(Recorder::default());
            let response = invoke(&loaded, &request, recorder.clone(), params.as_bytes());
            let calls: Vec<Value> = recorder
                .0
                .lock()
                .unwrap()
                .iter()
                .map(|(destination, payload)| {
                    json!({"op": "publish", "destination": destination,
                           "payload": String::from_utf8_lossy(payload)})
                })
                .collect();
            let actual = match response {
                Ok(bytes) => json!({"result": String::from_utf8_lossy(&bytes)}),
                Err((code, message)) => json!({"error": code, "message": message}),
            };
            let outcome = (calls == *case["calls"].as_array().unwrap()
                && actual == case["response"])
                .then_some(())
                .ok_or_else(|| "calls or response differ from the recorded case".to_owned());
            record(&format!("events-handle/{name}"), outcome);
            // A changed destination or response must be detected by the same comparison.
            let mut mutated = case.clone();
            mutated["response"] = json!({"result": "{}"});
            let detected = if actual == mutated["response"] {
                Err("mutation not detected".to_owned())
            } else {
                Ok(())
            };
            record(&format!("events-handle/{name}/mutation"), detected);
        }
        Ok(())
    })();
    if let Err(reason) = events {
        record("events-handle", Err(reason));
    }
    drop(compiler);
    drop(scratch);
    assert!(!directory.exists());
    assert!(failures.is_empty(), "profile blocked: {failures:?}");
    println!("{}", json!({"check": "profile_go", "status": "passed"}));
}
