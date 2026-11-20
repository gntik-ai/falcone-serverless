//! Prebuilt operator-run suite for the restricted cluster pod. Never builds a Go guest.
#[path = "support/profile_go.rs"]
mod built;

use falcone_serverless_worker::{
    adapters::{compat::TreeFile, compat_go::check_tree},
    artifact::supervisor::CompileLimits,
    cache,
    host::{self, Bindings, Context, HostError, Operation, Reply},
};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

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

#[test]
#[ignore = "UNVERIFIED: requires FS_TEST_CLUSTER=1 and FS_TOOLCHAIN_CACHE"]
fn built_go_components() {
    if std::env::var("FS_TEST_CLUSTER").as_deref() != Ok("1") {
        println!("UNVERIFIED: FS_TEST_CLUSTER=1 required; no Go guest executed");
        return;
    }
    let Some(components) = built::components() else {
        return;
    };
    let manifest: toml::Value = toml::from_str(built::MANIFEST).unwrap();
    // These are embedded source bytes, never compiled. Each still gets exactly its
    // documented diagnostic in the relocated pod binary before any positive is run.
    for (fixture, source) in manifest["negative"].as_array().unwrap().iter().zip([
        include_str!("../qualification/profiles/go/negative/native-library/main.go"),
        include_str!("../qualification/profiles/go/negative/gpu/main.go"),
        include_str!("../qualification/profiles/go/negative/process-spawn/main.go"),
        include_str!("../qualification/profiles/go/negative/install-hook/main.go"),
        include_str!("../qualification/profiles/go/negative/package-not-in-catalogue/main.go"),
        include_str!("../qualification/profiles/go/negative/unsupported-api/main.go"),
    ]) {
        let verdict = check_tree(&[TreeFile {
            path: "main.go",
            text: source,
        }]);
        assert_eq!(verdict.diagnostics().len(), 1);
        let d = &verdict.diagnostics()[0];
        assert_eq!(d.code, fixture["code"].as_str().unwrap());
        assert_eq!(d.path, fixture["file"].as_str().unwrap());
        assert_eq!(d.line as i64, fixture["line"].as_integer().unwrap());
    }
    let directory = std::env::temp_dir().join(format!("profile-go-{}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    let _scratch = Scratch(directory.clone());
    let exe = std::env::current_exe().unwrap();
    let parent = exe.parent().unwrap();
    let binaries = if parent.file_name().is_some_and(|n| n == "deps") {
        parent.parent().unwrap()
    } else {
        parent
    };
    let mut compiler =
        cache::Cache::new(directory, binaries.join("artifact-compile-child")).unwrap();
    for (name, input, expected) in [
        (
            "echo-json",
            include_str!("../qualification/profiles/go/examples/echo-json/input.json"),
            include_str!("../qualification/profiles/go/examples/echo-json/expected.json"),
        ),
        (
            "uppercase",
            include_str!("../qualification/profiles/go/examples/uppercase/input.json"),
            include_str!("../qualification/profiles/go/examples/uppercase/expected.json"),
        ),
        (
            "hash-sha256",
            include_str!("../qualification/profiles/go/examples/hash-sha256/input.json"),
            include_str!("../qualification/profiles/go/examples/hash-sha256/expected.json"),
        ),
    ] {
        let bytes = fs::read(components.join(format!("{name}.wasm")))
            .expect("missing trusted-built component");
        let raw = fs::read(components.join("raw").join(format!("{name}.wasm")))
            .expect("missing pre-composition TinyGo output");
        built::validate_fixture(&raw).unwrap();
        let imports = built::validate_component(&bytes).unwrap();
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
                Some(&imports),
            )
            .unwrap();
        let mut host = host::Host::new(Context::default(), Arc::new(NoBindings));
        host.set_fuel(100_000_000);
        host.set_until(Instant::now() + Duration::from_secs(30));
        let params: serde_json::Value = serde_json::from_str(input).unwrap();
        let execution = host::execute(&loaded, host, &serde_json::to_vec(&params).unwrap());
        let output = execution.result.expect("Go fixture handler failed");
        let actual: serde_json::Value =
            serde_json::from_slice(&output).expect("Go fixture returned invalid JSON");
        let expected: serde_json::Value = serde_json::from_str(expected).unwrap();
        assert_eq!(actual, expected, "recorded Go fixture output mismatch");
        println!("{name}: passed");
    }
    println!("{{\"check\":\"profile_go\",\"status\":\"passed\"}}");
}
