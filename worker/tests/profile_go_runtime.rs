//! Operator-run trusted Go build and execution. No default toolchain or guest build.
#[path = "support/go_wasi.rs"]
mod wasi_cases;
use falcone_serverless_worker::{
    artifact::{self, supervisor::CompileLimits},
    cache,
    host::{self, Context, Host},
};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};
use wasi_cases::Captured;
use wasmparser::{Parser, Payload};
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn scratch(name: &str) -> Scratch {
    let path = std::env::temp_dir().join(format!(
        "{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&path).unwrap();
    Scratch(path)
}
fn core_shape(bytes: &[u8]) -> Result<(), &'static str> {
    let (mut memories, mut reallocs) = (0, 0);
    for payload in Parser::new(0).parse_all(bytes) {
        match payload.map_err(|_| "invalid component")? {
            Payload::MemorySection(section) => memories += section.count(),
            Payload::ExportSection(section) => {
                for export in section {
                    let export = export.map_err(|_| "invalid core export")?;
                    if export.name == "cabi_realloc"
                        && export.kind == wasmparser::ExternalKind::Func
                    {
                        reallocs += 1;
                    }
                }
            }
            _ => {}
        }
    }
    if memories != 1 {
        return Err("fixture must contain exactly one linear memory");
    }
    if reallocs != 1 {
        return Err("fixture must contain exactly one cabi_realloc core export");
    }
    Ok(())
}
#[test]
fn duplicate_allocator_exports_and_adapter_memories_fail_closed() {
    let module = "(core module (memory 1) (func (export \"cabi_realloc\") (param i32 i32 i32 i32) (result i32) i32.const 0))";
    assert!(core_shape(&wat::parse_str(format!("(component {module})")).unwrap()).is_ok());
    assert_eq!(
        core_shape(
            &wat::parse_str(format!(
                "(component {module} (core module (func (export \"cabi_realloc\"))))"
            ))
            .unwrap()
        ),
        Err("fixture must contain exactly one cabi_realloc core export")
    );
    assert_eq!(
        core_shape(
            &wat::parse_str(format!("(component {module} (core module (memory 1)))")).unwrap()
        ),
        Err("fixture must contain exactly one linear memory")
    );
}

#[test]
#[ignore = "SKIPPED: operator-run, requires FS_TOOLCHAIN_CACHE in the trusted compiler image"]
fn trusted_build_options_and_fixtures() {
    if std::env::var_os("FS_TOOLCHAIN_CACHE").is_none() {
        println!(
            "SKIPPED: FS_TOOLCHAIN_CACHE unset; real TinyGo build is operator-run after merge, profile stays disabled and UNVERIFIED"
        );
        return;
    }
    let dir = scratch("go-build");
    let script =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("qualification/go/build-profile.sh");
    // Run the identical option probe and full build. The script creates probe/go.mod
    // before invoking the pinned TinyGo loader. A flag-only probe cannot qualify fixtures.
    let status = std::process::Command::new("bash")
        .arg(script)
        .env_remove("FS_OPTION_PROBE_ONLY")
        .env("FS_PROFILE_COMPONENTS", dir.0.join("components"))
        .status()
        .unwrap();
    assert!(status.success(), "trusted Go fixture build failed");
}

#[test]
#[ignore = "SKIPPED: requires FS_TOOLCHAIN_CACHE and trusted-image-built Go components"]
fn built_profile_components() {
    let Some(cache_root) = std::env::var_os("FS_TOOLCHAIN_CACHE") else {
        println!(
            "SKIPPED: FS_TOOLCHAIN_CACHE unset; no guest executed, Go remains disabled and UNVERIFIED"
        );
        return;
    };
    let components = std::env::var_os("FS_PROFILE_COMPONENTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(cache_root).join("profile-go"));
    let evidence = components.join("evidence");
    let manifest: toml::Value =
        toml::from_str(&fs::read_to_string(evidence.join("manifest.toml")).unwrap()).unwrap();
    let exe = std::env::current_exe().unwrap();
    let parent = exe.parent().unwrap();
    let binaries = if parent.file_name().is_some_and(|n| n == "deps") {
        parent.parent().unwrap()
    } else {
        parent
    };
    let dir = scratch("go-runtime");
    let mut compiler = cache::Cache::new(&dir.0, binaries.join("artifact-compile-child")).unwrap();
    for workload in manifest["workloads"].as_array().unwrap() {
        let name = workload["name"].as_str().unwrap();
        let bytes = fs::read(components.join(format!("{name}.wasm"))).unwrap();
        let validated = artifact::validate::validate(&bytes, bytes.len()).unwrap();
        let falcone = manifest["wit"]["interfaces"].as_array().unwrap();
        let wasi = manifest["host_wasi"]["interfaces"].as_array().unwrap();
        for import in &validated.imports {
            assert!(
                falcone
                    .iter()
                    .chain(wasi)
                    .any(|v| v.as_str() == Some(import)),
                "unexpected import"
            );
            if import.starts_with("falcone:") {
                assert!(
                    workload["imports"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|v| v.as_str() == Some(import))
                );
            }
        }
        println!("{name} imports: {:?}", validated.imports);
        core_shape(&bytes).unwrap();
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
                Some(&validated.imports),
            )
            .unwrap();
        let mut host = Host::new(Context::default(), Arc::new(Captured::default()));
        host.set_fuel(100_000_000);
        host.set_max_host_calls(10_000);
        host.set_until(Instant::now() + Duration::from_secs(30));
        let input = fs::read(evidence.join(workload["input"].as_str().unwrap())).unwrap();
        let expected: serde_json::Value = serde_json::from_slice(
            &fs::read(evidence.join(workload["expected"].as_str().unwrap())).unwrap(),
        )
        .unwrap();
        let output = host::execute(&loaded, host, &input).result.unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output).unwrap(),
            expected,
            "{name}"
        );
    }
    // Negative sources are checked, never executed, including in the restricted pod.
    for fixture in manifest["negative"].as_array().unwrap() {
        use falcone_serverless_worker::adapters::compat_go::{TreeFile, check_tree};
        let path = fixture["file"].as_str().unwrap();
        let text = fs::read_to_string(evidence.join(path)).unwrap();
        assert!(
            check_tree(&[TreeFile { path, text: &text }], None)
                .diagnostics()
                .iter()
                .any(|d| d.code == fixture["code"].as_str().unwrap()
                    && d.line == fixture["line"].as_integer().unwrap() as usize)
        );
    }
    // The same security criteria run locally and in the restricted pod.
    wasi_cases::csprng_is_linked_narrowly_and_bounded();
    wasi_cases::stdout_is_bounded_and_invocation_local();
    wasi_cases::one_memory_and_only_declared_functions_are_linked();
    println!(
        "Go runtime checks passed; qualification remains UNVERIFIED until artifact ID and SHA256 are recorded on #53"
    );
}
