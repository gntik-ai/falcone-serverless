#![cfg(target_os = "linux")]

#[path = "../src/artifact/mod.rs"]
mod artifact;
#[path = "../src/qualification/handler_fixtures.rs"]
pub mod handler_fixtures;

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
                "falcone:host/{}@0.2.0",
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
        "(component (type (func)) (import \"falcone:host/http@0.2.0\" (func (type 0))))",
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
    // Independent literal: `sha256sum handler_fixtures/modes.wasm`.
    let result = supervisor
        .compile(
            handler_fixtures::MODES,
            limits(),
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();
    assert!(
        result
            .as_str()
            .contains("498b0590d069902c5942d0dc53c3f95011463276bb683b65f4fa2c1f92d6e436")
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
        "(component (type (instance)) (import \"other:host/http@0.2.0\" (instance (type 0))))",
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
        // The echo guest's items come first so the handler gate accepts the component.
        let echo = include_str!("../src/qualification/handler_fixtures/echo.wat")
            .trim()
            .strip_prefix("(component")
            .and_then(|wat| wat.strip_suffix(')'))
            .unwrap();
        wat::parse_str(format!(
            "(component{echo}\n(core module {}))",
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

// --- falcone:host/handler@0.2.0 export gate -------------------------------------------

const HANDLER: &str = "falcone:host/handler@0.2.0";
const ERROR: &str = r#"(variant (case "invalid-input" string) (case "failed" string))"#;

/// One handler component, varied in a single dimension per test. The core module's
/// signature fits every probed `handle` type (two flat params, one flat or pointer result).
struct Probe<'a> {
    imports: &'a str,
    error: &'a str,
    param_name: &'a str,
    param: &'a str,
    result: &'a str,
    export: &'a str,
    extra: &'a str,
}

impl Default for Probe<'_> {
    fn default() -> Self {
        Probe {
            imports: "",
            error: ERROR,
            param_name: "params",
            param: "(list u8)",
            result: "(result (list u8) (error $error))",
            export: HANDLER,
            extra: "",
        }
    }
}

impl Probe<'_> {
    fn wasm(&self) -> Vec<u8> {
        wat::parse_str(format!(
            r#"(component
  {imports}
  (core module $guest
    (memory (export "memory") 1)
    (func (export "realloc") (param i32 i32 i32 i32) (result i32) i32.const 8)
    (func (export "handle") (param i32 i32) (result i32) i32.const 0))
  (core instance $guest (instantiate $guest))
  (type $error {error})
  (func $handle (param "{param_name}" {param}) (result {result})
    (canon lift (core func $guest "handle") (memory $guest "memory") (realloc (func $guest "realloc"))))
  (instance $handler (export "handler-error" (type $error)) (export "handle" (func $handle)))
  (export "{export}" (instance $handler))
  {extra})"#,
            imports = self.imports,
            error = self.error,
            param_name = self.param_name,
            param = self.param,
            result = self.result,
            export = self.export,
            extra = self.extra,
        ))
        .unwrap()
    }
}

fn gate(bytes: &[u8]) -> Result<(), artifact::Error> {
    artifact::validate::validate(bytes, bytes.len()).map(drop)
}

fn code(bytes: &[u8]) -> &'static str {
    gate(bytes)
        .expect_err("the gate must reject this component")
        .code
}

#[test]
fn handler_gate_accepts_the_exact_export_with_only_falcone_host_imports() {
    gate(&Probe::default().wasm()).unwrap();
    gate(&small()).unwrap();
    for fixture in [
        handler_fixtures::ECHO,
        handler_fixtures::MODES,
        handler_fixtures::START_LOGS_WITH_HANDLE,
    ] {
        gate(fixture).unwrap();
    }
}

#[test]
fn handler_fixture_binaries_match_their_wat_sources() {
    macro_rules! pair {
        ($name:literal, $binary:expr) => {
            (
                $name,
                include_str!(concat!(
                    "../src/qualification/handler_fixtures/",
                    $name,
                    ".wat"
                )),
                $binary,
            )
        };
    }
    for (name, source, binary) in [
        pair!("echo", handler_fixtures::ECHO),
        pair!("modes", handler_fixtures::MODES),
        pair!(
            "start-logs-without-handle",
            handler_fixtures::START_LOGS_WITHOUT_HANDLE
        ),
        pair!(
            "start-logs-with-handle",
            handler_fixtures::START_LOGS_WITH_HANDLE
        ),
        pair!("wrong-result", handler_fixtures::WRONG_RESULT),
    ] {
        assert_eq!(
            wat::parse_str(source).unwrap(),
            binary,
            "{name}.wasm is stale"
        );
    }
}

#[test]
fn import_gate_still_rejects_wasi_and_the_previous_interface_version() {
    for import in [
        r#"(import "wasi:cli/environment@0.2.0" (instance))"#,
        r#"(import "wasi:filesystem/types@0.2.0" (instance))"#,
        r#"(import "falcone:host/http@0.1.0" (instance))"#,
        r#"(import "falcone:host/http@0.3.0" (instance))"#,
    ] {
        let error = gate(
            &Probe {
                imports: import,
                ..Probe::default()
            }
            .wasm(),
        )
        .unwrap_err();
        assert_eq!(error.code, "undeclared_import", "{import}");
        assert!(error.import.is_some() && error.export.is_none(), "{import}");
    }
}

#[test]
fn a_missing_or_misnamed_export_is_missing_handler_export() {
    assert_eq!(code(b"\0asm\x0d\0\x01\0"), "missing_handler_export");
    for export in [
        "falcone:host/handler@0.1.0",
        "falcone:host/handler@0.3.0",
        "falcone:host/handler",
        "handler",
        "wasi:cli/run@0.2.0",
    ] {
        let error = gate(
            &Probe {
                export,
                ..Probe::default()
            }
            .wasm(),
        )
        .unwrap_err();
        assert_eq!(error.code, "missing_handler_export", "{export}");
        assert_eq!(error.export.as_deref(), Some(export));
    }
    // The right name, but not an instance.
    let function = wat::parse_str(
        r#"(component
  (core module $guest (func (export "f")))
  (core instance $guest (instantiate $guest))
  (func $f (canon lift (core func $guest "f")))
  (export "falcone:host/handler@0.2.0" (func $f)))"#,
    )
    .unwrap();
    assert_eq!(code(&function), "missing_handler_export");
}

#[test]
fn an_extra_top_level_export_is_rejected_with_a_bounded_diagnostic() {
    let extra = Probe {
        extra: r#"(export "extra" (instance $handler))"#,
        ..Probe::default()
    };
    let error = gate(&extra.wasm()).unwrap_err();
    assert_eq!(error.code, "missing_handler_export");
    assert_eq!(error.export.as_deref(), Some("extra"));
    let long = "x".repeat(4096);
    let extra = format!(r#"(export "{long}" (instance $handler))"#);
    let error = gate(
        &Probe {
            extra: &extra,
            ..Probe::default()
        }
        .wasm(),
    )
    .unwrap_err();
    assert_eq!(error.export.unwrap().chars().count(), 256);
}

#[test]
fn an_empty_or_incomplete_handler_instance_is_an_invalid_signature() {
    let empty = wat::parse_str(
        r#"(component (instance $handler) (export "falcone:host/handler@0.2.0" (instance $handler)))"#,
    )
    .unwrap();
    assert_eq!(code(&empty), "invalid_handler_signature");
    assert_eq!(
        code(handler_fixtures::START_LOGS_WITHOUT_HANDLE),
        "invalid_handler_signature"
    );
    // A function under another name is not `handle`; a module is not a function.
    let renamed = wat::parse_str(
        r#"(component
  (core module $guest
    (memory (export "memory") 1)
    (func (export "realloc") (param i32 i32 i32 i32) (result i32) i32.const 8)
    (func (export "handle") (param i32 i32) (result i32) i32.const 0))
  (core instance $guest (instantiate $guest))
  (func $handle (param "params" (list u8)) (result (list u8))
    (canon lift (core func $guest "handle") (memory $guest "memory") (realloc (func $guest "realloc"))))
  (instance $handler (export "handler" (func $handle)))
  (export "falcone:host/handler@0.2.0" (instance $handler)))"#,
    )
    .unwrap();
    assert_eq!(code(&renamed), "invalid_handler_signature");
}

#[test]
fn a_handle_with_the_wrong_parameter_or_result_type_is_an_invalid_signature() {
    let other_error = r#"(variant (case "invalid-input" string) (case "crashed" string))"#;
    let payloadless = r#"(variant (case "invalid-input" string) (case "failed"))"#;
    let swapped = r#"(variant (case "failed" string) (case "invalid-input" string))"#;
    let three =
        r#"(variant (case "invalid-input" string) (case "failed" string) (case "other" string))"#;
    for (name, probe) in [
        (
            "param string",
            Probe {
                param: "string",
                ..Probe::default()
            },
        ),
        (
            "param list u16",
            Probe {
                param: "(list u16)",
                ..Probe::default()
            },
        ),
        (
            "param renamed",
            Probe {
                param_name: "input",
                ..Probe::default()
            },
        ),
        (
            "result u32",
            Probe {
                result: "u32",
                ..Probe::default()
            },
        ),
        (
            "result list",
            Probe {
                result: "(list u8)",
                ..Probe::default()
            },
        ),
        (
            "result string ok",
            Probe {
                result: "(result string (error $error))",
                ..Probe::default()
            },
        ),
        (
            "result no error",
            Probe {
                result: "(result (list u8))",
                ..Probe::default()
            },
        ),
        (
            "result string error",
            Probe {
                result: "(result (list u8) (error string))",
                ..Probe::default()
            },
        ),
        (
            "error case renamed",
            Probe {
                error: other_error,
                ..Probe::default()
            },
        ),
        (
            "error case payloadless",
            Probe {
                error: payloadless,
                ..Probe::default()
            },
        ),
        (
            "error cases reordered",
            Probe {
                error: swapped,
                ..Probe::default()
            },
        ),
        (
            "error extra case",
            Probe {
                error: three,
                ..Probe::default()
            },
        ),
    ] {
        assert_eq!(code(&probe.wasm()), "invalid_handler_signature", "{name}");
    }
    assert_eq!(
        code(handler_fixtures::WRONG_RESULT),
        "invalid_handler_signature"
    );
}

#[test]
fn a_second_function_in_the_handler_instance_is_rejected() {
    let bytes = wat::parse_str(
        r#"(component
  (core module $guest
    (memory (export "memory") 1)
    (func (export "realloc") (param i32 i32 i32 i32) (result i32) i32.const 8)
    (func (export "handle") (param i32 i32) (result i32) i32.const 0))
  (core instance $guest (instantiate $guest))
  (type $error (variant (case "invalid-input" string) (case "failed" string)))
  (type $reply (result (list u8) (error $error)))
  (func $handle (param "params" (list u8)) (result $reply)
    (canon lift (core func $guest "handle") (memory $guest "memory") (realloc (func $guest "realloc"))))
  (instance $handler (export "handler-error" (type $error)) (export "handle" (func $handle)) (export "other" (func $handle)))
  (export "falcone:host/handler@0.2.0" (instance $handler)))"#,
    )
    .unwrap();
    assert_eq!(code(&bytes), "invalid_handler_signature");
}

#[test]
fn nested_component_exports_do_not_satisfy_the_gate() {
    let nested = wat::parse_str(
        r#"(component
  (component $inner
    (core module $guest (func (export "f")))
    (core instance $guest (instantiate $guest))
    (instance $handler)
    (export "falcone:host/handler@0.2.0" (instance $handler))))"#,
    )
    .unwrap();
    assert_eq!(code(&nested), "missing_handler_export");
}

#[test]
fn interface_version_is_0_2_0_so_a_0_1_0_native_cache_can_never_match() {
    use artifact::cache_key::{CacheKey, ENGINE_CONFIG, ENGINE_VERSION, INTERFACE_VERSION};
    assert_eq!(INTERFACE_VERSION, "falcone:host@0.2.0");
    let key = |interface| {
        CacheKey::new([7; 32], ENGINE_VERSION, "x86_64", ENGINE_CONFIG, interface).unwrap()
    };
    assert_ne!(key(INTERFACE_VERSION), key("falcone:host@0.1.0"));
}

#[test]
fn catalog_bases_keep_the_import_policy_but_need_no_handler() {
    use artifact::validate::{validate, validate_catalog};
    // A base component is composed into function components later and exports no handler.
    let base = b"\0asm\x0d\0\x01\0";
    validate_catalog(base, base.len()).unwrap();
    assert_eq!(
        validate(base, base.len()).unwrap_err().code,
        "missing_handler_export"
    );
    let wasi = Probe {
        imports: r#"(import "wasi:cli/environment@0.2.0" (instance))"#,
        ..Probe::default()
    }
    .wasm();
    assert_eq!(
        validate_catalog(&wasi, wasi.len()).unwrap_err().code,
        "undeclared_import"
    );
}
