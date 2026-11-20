//! Operator-only trusted-built TinyGo execution. Ordinary CI never builds a toolchain.
use falcone_serverless_worker::{
    adapters::{compat::TreeFile, compat_go},
    artifact::{supervisor::CompileLimits, validate::validate},
    cache,
    host::{self, Bindings, Context, Host, HostError, Operation, Reply},
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

const MANIFEST: &str = include_str!("../qualification/profiles/go/manifest.toml");

// Count definitions throughout all core modules, including any adapter. A second
// realloc export or memory anywhere in a composed component is rejected.
fn abi_shape(bytes: &[u8]) -> Result<(), &'static str> {
    use wasmparser::{ExternalKind, Parser, Payload};
    let (mut memories, mut reallocs) = (0, 0);
    for payload in Parser::new(0).parse_all(bytes) {
        match payload.map_err(|_| "invalid component")? {
            Payload::MemorySection(section) => memories += section.count(),
            Payload::ExportSection(section) => {
                for export in section {
                    let export = export.map_err(|_| "invalid core export")?;
                    if export.kind == ExternalKind::Func && export.name == "cabi_realloc" {
                        reallocs += 1;
                    }
                }
            }
            _ => {}
        }
    }
    if memories != 1 {
        return Err("expected exactly one linear memory");
    }
    if reallocs != 1 {
        return Err("expected exactly one core cabi_realloc export");
    }
    Ok(())
}

#[test]
fn adapter_memory_and_duplicate_allocator_exports_are_detected() {
    let module = "(core module (memory 1) (func (export \"cabi_realloc\")))";
    let one = wat::parse_str(format!("(component {module})")).unwrap();
    abi_shape(&one).unwrap();
    let two = wat::parse_str(format!("(component {module} {module})")).unwrap();
    assert_eq!(abi_shape(&two), Err("expected exactly one linear memory"));
    let second = "(core module (func (export \"cabi_realloc\")))";
    let two = wat::parse_str(format!("(component {module} {second})")).unwrap();
    assert_eq!(
        abi_shape(&two),
        Err("expected exactly one core cabi_realloc export")
    );
}

struct NoBindings;
impl Bindings for NoBindings {
    fn call(&self, _: &Context, _: Operation) -> Result<Reply, HostError> {
        Err(HostError::Denied)
    }
}
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn receipt(directory: &Path, path: &str) -> Vec<u8> {
    let bytes = fs::read(directory.join(path)).expect("trusted bundle file missing");
    let sum = format!("{:x}", Sha256::digest(&bytes));
    let receipt =
        fs::read_to_string(directory.join("SHA256SUMS")).expect("trusted build receipt missing");
    assert!(
        receipt.lines().any(|line| {
            line.split_once("  ")
                .is_some_and(|(sha, file)| sha == sum && file.trim_start_matches("./") == path)
        }),
        "trusted bundle sha256 mismatch: {path}"
    );
    bytes
}

#[test]
#[ignore = "UNVERIFIED: requires FS_TOOLCHAIN_CACHE and trusted-image-built Go components"]
fn built_profile_components() {
    let Some(cache_root) = std::env::var_os("FS_TOOLCHAIN_CACHE") else {
        println!("UNVERIFIED: FS_TOOLCHAIN_CACHE unset; no Go fixture built or executed");
        return;
    };
    let directory = std::env::var_os("FS_PROFILE_COMPONENTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(cache_root).join("profile"));
    assert_eq!(
        receipt(&directory, "evidence/manifest.toml"),
        MANIFEST.as_bytes(),
        "bundle manifest differs from runner"
    );
    let manifest: toml::Value = toml::from_str(MANIFEST).unwrap();
    assert!(compat_go::check_tree(&[], MANIFEST).is_empty());
    // Negative sources never reach the compiler. Their rejection is part of the pod's
    // success, rather than relying on a static check made outside the restricted pod.
    for (name, code) in [
        ("native-library", "WASM-NATIVE-LIBRARY"),
        ("process-spawn", "WASM-PROCESS-SPAWN"),
        ("install-hook", "WASM-INSTALL-HOOK"),
        ("gpu", "WASM-GPU"),
        ("package-not-in-catalogue", "WASM-PACKAGE-NOT-IN-CATALOGUE"),
        ("unsupported-api", "WASM-UNSUPPORTED-API"),
    ] {
        let path = format!("evidence/negative/{name}/main.go");
        let source = String::from_utf8(receipt(&directory, &path)).unwrap();
        let diagnostics = compat_go::check_tree(
            &[TreeFile {
                path: &path,
                text: &source,
            }],
            MANIFEST,
        );
        assert!(
            diagnostics.iter().any(|d| d.code == code),
            "negative fixture was not rejected: {name}"
        );
        assert!(
            !directory.join(format!("{name}.wasm")).exists(),
            "negative fixture must never be built"
        );
    }
    let exe = std::env::current_exe().unwrap();
    let parent = exe.parent().unwrap();
    let binaries = if parent.file_name().is_some_and(|n| n == "deps") {
        parent.parent().unwrap()
    } else {
        parent
    };
    // Includes the random freshness, size, narrow linking, fuel/deadline/cancellation
    // checks embedded in the shared capability qualification binary.
    let gates = std::process::Command::new(binaries.join("compile-cache-qualification"))
        .arg("--capability-gates")
        .output()
        .expect("capability qualification executable missing");
    assert!(
        gates.status.success(),
        "host capability qualification failed"
    );
    let scratch = Scratch(std::env::temp_dir().join(format!("profile-go-{}", std::process::id())));
    fs::create_dir(&scratch.0).unwrap();
    let mut compiler =
        cache::Cache::new(&scratch.0, binaries.join("artifact-compile-child")).unwrap();
    for workload in manifest["workloads"].as_array().unwrap() {
        let name = workload["name"].as_str().unwrap();
        let bytes = receipt(&directory, &format!("{name}.wasm"));
        abi_shape(&bytes).unwrap();
        let artifact = validate(&bytes, 64 * 1024 * 1024).unwrap();
        let allowed = &manifest["wit"]["interfaces"].as_array().unwrap();
        assert!(
            artifact
                .imports
                .iter()
                .all(|import| allowed.iter().any(|a| a.as_str() == Some(import)))
        );
        let declared = workload["imports"].as_array().unwrap();
        assert!(
            artifact
                .imports
                .iter()
                .all(|import| declared.iter().any(|a| a.as_str() == Some(import)))
        );
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
        let mut host = Host::new(Context::default(), Arc::new(NoBindings));
        host.set_fuel(10_000_000);
        host.set_max_host_calls(1024);
        host.set_until(Instant::now() + Duration::from_secs(30));
        let result = host::execute(
            &loaded,
            host,
            workload["input"].as_str().unwrap().as_bytes(),
        );
        let result = result
            .result
            .expect("Go fixture failed to instantiate or execute under memories(1)");
        let actual: serde_json::Value =
            serde_json::from_slice(&result).expect("invalid fixture JSON");
        let expected: serde_json::Value =
            serde_json::from_str(workload["expected"].as_str().unwrap()).unwrap();
        assert!(actual == expected, "fixture output mismatch: {name}");
    }
    println!(
        "{}",
        serde_json::json!({"check":"profile_go", "status":"passed"})
    );
}
