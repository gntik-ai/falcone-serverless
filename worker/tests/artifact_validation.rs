#![cfg(target_os = "linux")]

#[path = "../src/artifact/mod.rs"]
mod artifact;

#[test]
fn bounded_transfer_preserves_destination_on_rejection() {
    let mut destination = b"unchanged".to_vec();
    let oversized = vec![42; 1_048_577];
    let error = artifact::transfer::copy(&oversized, &mut destination).unwrap_err();
    assert_eq!(error.code, "buffer_limit_exceeded");
    assert_eq!(destination, b"unchanged");
    let exact = vec![42; 1_048_576];
    artifact::transfer::copy(&exact, &mut destination).unwrap();
    assert_eq!(destination, exact);
    let readme = include_str!("../wit/README.md");
    assert!(readme.contains("max-guest-to-host-bytes: 1048576"));
    assert!(readme.contains("max-host-to-guest-bytes: 1048576"));
}

fn small() -> Vec<u8> {
    wat::parse_str(include_str!("fixtures/was-02/allowed.wat")).unwrap()
}

#[test]
fn raw_component_accepts_exact_world_function_interfaces() {
    use std::collections::BTreeSet;
    use wit_parser::{Resolve, WorldItem};
    let mut resolve = Resolve::default();
    let (package, _) = resolve.push_path("wit/host.wit").unwrap();
    let package = &resolve.packages[package];
    let world = &resolve.worlds[package.worlds["function"]];
    let expected: BTreeSet<_> = world
        .imports
        .values()
        .map(|item| {
            let WorldItem::Interface { id, .. } = item else {
                panic!("interface import")
            };
            format!(
                "falcone:host/{}@0.1.0",
                resolve.interfaces[*id].name.as_ref().unwrap()
            )
        })
        .collect();
    assert_eq!(expected.len(), 8);
    assert_eq!(
        artifact::validate::ALLOWED_IMPORTS
            .iter()
            .map(|s| s.to_string())
            .collect::<BTreeSet<_>>(),
        expected
    );
    let bytes = small();
    let validated = artifact::validate::validate(&bytes, bytes.len()).unwrap();
    assert_eq!(
        validated.imports.iter().cloned().collect::<BTreeSet<_>>(),
        expected
    );
}

#[test]
fn validation_order_and_input_boundary() {
    use artifact::validate::validate;
    assert_eq!(
        validate(b"not wasm", 7).unwrap_err().code,
        "compile_size_limit_exceeded"
    );
    assert_eq!(
        validate(include_bytes!("fixtures/was-02/native.elf"), 100)
            .unwrap_err()
            .code,
        "precompiled_rejected"
    );
    assert_eq!(
        validate(b"\0asm\x0d\0\x01\0\xff", 100).unwrap_err().code,
        "invalid_module"
    );
    assert_eq!(
        validate(&wat::parse_str("(module)").unwrap(), 100)
            .unwrap_err()
            .code,
        "invalid_module"
    );
    let mut exact = small();
    // A custom section named 'p' with 62 padding bytes: 64 payload bytes.
    exact.extend_from_slice(&[0, 64, 1, b'p']);
    exact.extend_from_slice(&[0; 62]);
    let limit = exact.len();
    validate(&exact, limit).unwrap();
    exact.push(0);
    assert_eq!(
        validate(&exact, limit).unwrap_err().code,
        "compile_size_limit_exceeded"
    );
    let forbidden = wat::parse_str("(component (type (instance)) (import \"wasi:filesystem/types@0.2.0\" (instance (type 0))))").unwrap();
    let error = validate(&forbidden, 1024).unwrap_err();
    assert_eq!(error.code, "undeclared_import");
    assert_eq!(error.import.as_deref(), Some("wasi:filesystem/types@0.2.0"));
    let wrong_kind = wat::parse_str(
        "(component (type (func)) (import \"falcone:host/http@0.1.0\" (func (type 0))))",
    )
    .unwrap();
    assert_eq!(
        validate(&wrong_kind, 1024).unwrap_err().code,
        "undeclared_import"
    );
    // Full validation must catch invalid code even inside a nested core module.
    let invalid_nested = wat::parse_str("(component (core module (func (result i32))))").unwrap();
    assert_eq!(
        validate(&invalid_nested, 1024).unwrap_err().code,
        "invalid_module"
    );
}

#[test]
fn cache_identity_requires_and_distinguishes_every_component() {
    use artifact::cache_key::{CacheKey, ENGINE_VERSION};
    let digest = [0xab; 32];
    let original =
        CacheKey::new(digest, "49.0.5", "x86_64", "epoch=true;fuel=true", "0.1.0").unwrap();
    assert!(
        original
            .as_str()
            .contains("abababababababababababababababababababababababababababababababab")
    );
    for (digest, engine, target, config, interface) in [
        (
            [0xcd; 32],
            "49.0.5",
            "x86_64",
            "epoch=true;fuel=true",
            "0.1.0",
        ),
        (digest, "49.0.6", "x86_64", "epoch=true;fuel=true", "0.1.0"),
        (digest, "49.0.5", "aarch64", "epoch=true;fuel=true", "0.1.0"),
        (digest, "49.0.5", "x86_64", "epoch=false;fuel=true", "0.1.0"),
        (digest, "49.0.5", "x86_64", "epoch=true;fuel=true", "0.2.0"),
    ] {
        assert_ne!(
            original,
            CacheKey::new(digest, engine, target, config, interface).unwrap()
        );
    }
    for missing in 0..4 {
        let mut parts = ["49.0.5", "x86_64", "epoch=true;fuel=true", "0.1.0"];
        parts[missing] = "";
        assert!(CacheKey::new(digest, parts[0], parts[1], parts[2], parts[3]).is_err());
    }
    let versions: toml::Value =
        toml::from_str(include_str!("../qualification/was-01/versions.toml")).unwrap();
    assert_eq!(
        ENGINE_VERSION,
        versions["wasmtime_version"].as_str().unwrap()
    );
}

fn limits() -> artifact::supervisor::CompileLimits {
    artifact::supervisor::CompileLimits {
        input_bytes: 64 * 1024 * 1024,
        cpu_seconds: 20,
        memory_bytes: 2 * 1024 * 1024 * 1024,
        wall_clock_deadline: std::time::Instant::now() + std::time::Duration::from_secs(60),
    }
}

fn supervisor() -> artifact::supervisor::Supervisor {
    artifact::supervisor::Supervisor::new(env!("CARGO_BIN_EXE_artifact-compile-child"))
}

#[test]
fn validated_component_compiles_in_child_with_digest_key() {
    let mut supervisor = supervisor();
    let result = supervisor
        .compile(
            &small(),
            limits(),
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(supervisor.spawn_count(), 1);
    assert!(supervisor.last_child_status().unwrap().unwrap().success());
    assert!(result.as_str().starts_with("sha256:"));
    let empty = b"\0asm\x0d\0\x01\0";
    let result = supervisor
        .compile(empty, limits(), &std::sync::atomic::AtomicBool::new(false))
        .unwrap();
    assert!(
        result
            .as_str()
            .contains("24eaf3439a60c96764c4f2c92a0a81b52bd4fe5d5b91f090184a0f6486b1bf29")
    );
}

#[test]
fn rejected_input_never_spawns_a_compiler() {
    use wasmtime as engine;
    let engine = engine::Engine::default();
    let native = engine.precompile_component(&small()).unwrap();
    let core = wat::parse_str("(module)").unwrap();
    let malformed = b"\0asm\x0d\0\x01\0\xff";
    let unlisted = wat::parse_str(
        "(component (type (instance)) (import \"other:host/http@0.1.0\" (instance (type 0))))",
    )
    .unwrap();
    let oversized = vec![0; limits().input_bytes + 1];
    let mut supervisor = supervisor();
    for (input, expected) in [
        (
            include_bytes!("fixtures/was-02/native.elf").as_slice(),
            "precompiled_rejected",
        ),
        (native.as_slice(), "precompiled_rejected"),
        (malformed.as_slice(), "invalid_module"),
        (core.as_slice(), "invalid_module"),
        (unlisted.as_slice(), "undeclared_import"),
        (oversized.as_slice(), "compile_size_limit_exceeded"),
    ] {
        let error = supervisor
            .compile(input, limits(), &std::sync::atomic::AtomicBool::new(false))
            .unwrap_err();
        assert_eq!(error.code, expected);
        assert_eq!(supervisor.spawn_count(), 0);
    }
}

fn heavy() -> &'static [u8] {
    static HEAVY: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    HEAVY.get_or_init(|| {
        let body = "i32.const 3 i32.mul i32.const 1 i32.add\n".repeat(8192);
        let function =
            include_str!("fixtures/was-02/heavy-function.wat").replace(";; OPERATIONS", &body);
        wat::parse_str(format!(
            "(component (core module {}))",
            function.repeat(128)
        ))
        .unwrap()
    })
}

fn assert_reaped(supervisor: &mut artifact::supervisor::Supervisor) {
    assert!(
        supervisor.last_child_status().unwrap().is_some(),
        "child must be reaped before returning"
    );
}

fn assert_recovery(
    supervisor: &mut artifact::supervisor::Supervisor,
    limits: artifact::supervisor::CompileLimits,
) {
    supervisor
        .compile(&small(), limits, &std::sync::atomic::AtomicBool::new(false))
        .unwrap();
    assert!(supervisor.last_child_status().unwrap().unwrap().success());
}

#[test]
fn cpu_budget_bounds_real_compilation() {
    let mut supervisor = supervisor();
    let mut budget = limits();
    budget.cpu_seconds = 1;
    assert_recovery(&mut supervisor, budget);
    let error = supervisor
        .compile(heavy(), budget, &std::sync::atomic::AtomicBool::new(false))
        .unwrap_err();
    assert_eq!(error.code, "compile_cpu_limit_exceeded");
    assert_reaped(&mut supervisor);
    assert_recovery(&mut supervisor, budget);
}

#[test]
fn address_space_budget_bounds_real_compilation() {
    let mut supervisor = supervisor();
    let mut budget = limits();
    // Test-only virtual-address-space budget; the small fixture fits with margin.
    budget.memory_bytes = 64 * 1024 * 1024;
    assert_recovery(&mut supervisor, budget);
    let error = supervisor
        .compile(heavy(), budget, &std::sync::atomic::AtomicBool::new(false))
        .unwrap_err();
    assert_eq!(error.code, "compile_memory_limit_exceeded");
    assert_reaped(&mut supervisor);
    assert_recovery(&mut supervisor, budget);
}

#[test]
fn deadline_reaps_and_allows_recovery() {
    let mut supervisor = supervisor();
    let bytes = heavy();
    let mut budget = limits();
    budget.wall_clock_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let error = supervisor
        .compile(bytes, budget, &std::sync::atomic::AtomicBool::new(false))
        .unwrap_err();
    assert_eq!(error.code, "compile_deadline_exceeded");
    assert_reaped(&mut supervisor);
    assert_recovery(&mut supervisor, limits());
}

#[test]
fn cancellation_reaps_and_allows_recovery() {
    let mut supervisor = supervisor();
    let bytes = heavy();
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let spawned = supervisor.spawn_counter();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let wait_until = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while spawned.load(std::sync::atomic::Ordering::Acquire) == 0 {
                assert!(
                    std::time::Instant::now() < wait_until,
                    "expected child spawn"
                );
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            cancelled.store(true, std::sync::atomic::Ordering::Release);
        });
        let error = supervisor.compile(bytes, limits(), &cancelled).unwrap_err();
        assert_eq!(error.code, "compile_cancelled");
    });
    assert_reaped(&mut supervisor);
    assert_recovery(&mut supervisor, limits());
}

#[test]
fn native_cache_loading_is_statically_forbidden() {
    use std::{collections::BTreeSet, fs, path::Path};
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let config: toml::Value =
        toml::from_str(&fs::read_to_string(root.join("clippy.toml")).unwrap()).unwrap();
    let paths: BTreeSet<_> = config["disallowed-methods"]
        .as_array()
        .unwrap()
        .iter()
        .map(|path| path.as_str().unwrap())
        .collect();
    for ty in ["wasmtime::Module", "wasmtime::component::Component"] {
        for method in ["deserialize", "deserialize_raw", "deserialize_file"] {
            assert!(paths.contains(format!("{ty}::{method}").as_str()));
        }
    }
    assert!(paths.contains("wasmtime::Module::deserialize_open_file"));
    fn scan(path: &Path) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                scan(&path);
            } else {
                let source = fs::read_to_string(path).unwrap();
                assert!(!source.contains("deserialize"));
                assert!(!source.contains("wasmtime::"));
            }
        }
    }
    scan(&root.join("worker/src/artifact"));
}

struct ExitFixture(std::path::PathBuf);

impl ExitFixture {
    // Mock only the OS process boundary to pin all signal/status mappings.
    fn new(outcome: &str) -> Self {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("was02-exit-{}-{id}", std::process::id()));
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o700)
            .open(&path)
            .unwrap();
        writeln!(
            file,
            "#!/bin/sh\nwhile IFS= read -r line; do :; done\n{outcome}"
        )
        .unwrap();
        Self(path)
    }
}

impl Drop for ExitFixture {
    fn drop(&mut self) {
        std::fs::remove_file(&self.0).unwrap();
    }
}

#[test]
fn child_exit_mapping_and_unknown_failures_are_fail_closed() {
    for (outcome, expected) in [
        ("kill -24 $$", "compile_cpu_limit_exceeded"),
        ("kill -9 $$", "compile_cpu_limit_exceeded"),
        ("kill -6 $$", "compile_memory_limit_exceeded"),
        ("exit 66", "compile_memory_limit_exceeded"),
        ("kill -15 $$", "compile_failed"),
        ("exit 65", "compile_failed"),
        ("exit 77", "compile_failed"),
    ] {
        let fixture = ExitFixture::new(outcome);
        let mut supervisor = artifact::supervisor::Supervisor::new(&fixture.0);
        let error = supervisor
            .compile(
                &small(),
                limits(),
                &std::sync::atomic::AtomicBool::new(false),
            )
            .unwrap_err();
        assert_eq!(error.code, expected, "{outcome}");
        assert_reaped(&mut supervisor);
    }
}

#[test]
fn budgets_are_mandatory_and_applied_before_input_reaches_child() {
    for (cpu, memory, input) in [
        (0, 1, 1024),
        (1, 0, 1024),
        (1, 1, 0),
        (u64::MAX, 1, 1024),
        (1, u64::MAX, 1024),
    ] {
        let mut supervisor = supervisor();
        let mut budget = limits();
        budget.cpu_seconds = cpu;
        budget.memory_bytes = memory;
        budget.input_bytes = input;
        assert!(
            supervisor
                .compile(&small(), budget, &std::sync::atomic::AtomicBool::new(false))
                .is_err()
        );
        assert_eq!(supervisor.spawn_count(), 0);
    }
    let fixture = ExitFixture::new(
        "if ulimit -t 21 2>/dev/null; then exit 65; fi\nif ulimit -v 2097153 2>/dev/null; then exit 65; fi\nif ulimit -c 1 2>/dev/null; then exit 65; fi\nexit 0",
    );
    let mut supervisor = artifact::supervisor::Supervisor::new(&fixture.0);
    supervisor
        .compile(
            &small(),
            limits(),
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();
    assert_reaped(&mut supervisor);
    let missing = std::env::temp_dir().join(format!("was02-missing-{}", std::process::id()));
    let mut supervisor = artifact::supervisor::Supervisor::new(missing);
    let error = supervisor
        .compile(
            &small(),
            limits(),
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap_err();
    assert_eq!(error.code, "compile_failed");
    assert_eq!(supervisor.spawn_count(), 0);
}
