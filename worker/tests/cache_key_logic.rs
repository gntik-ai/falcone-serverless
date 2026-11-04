#![cfg(target_os = "linux")]

#[path = "../src/artifact/mod.rs"]
pub mod artifact;
#[path = "../src/cache/mod.rs"]
pub mod cache;
#[path = "../src/qualification/compile_cache_fixtures.rs"]
pub mod fixtures;

use artifact::supervisor::{CompileLimits, Supervisor};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

fn limits() -> CompileLimits {
    CompileLimits {
        input_bytes: 64 * 1024 * 1024,
        cpu_seconds: 120,
        memory_bytes: 4 * 1024 * 1024 * 1024,
        wall_clock_deadline: Instant::now() + Duration::from_secs(300),
    }
}

fn small() -> Vec<u8> {
    wat::parse_str(include_str!("fixtures/was-02/allowed.wat")).unwrap()
}

fn supervisor() -> Supervisor {
    Supervisor::new(env!("CARGO_BIN_EXE_artifact-compile-child"))
}

#[test]
fn trusted_child_returns_native_output_after_success_and_reap() {
    let mut child = supervisor();
    let (key, bytes) = child
        .compile_output(&small(), limits(), &AtomicBool::new(false))
        .unwrap();
    assert!(key.as_str().starts_with("sha256:"));
    assert!(bytes.starts_with(b"\x7fELF"));
    assert_eq!(child.spawn_count(), 1);
    assert!(child.last_child_status().unwrap().unwrap().success());
}

struct Store(std::path::PathBuf);

impl Store {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("was02-cache-{}-{id}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn cache(&self) -> cache::Cache {
        cache::Cache::new(&self.0, env!("CARGO_BIN_EXE_artifact-compile-child")).unwrap()
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn verified_cache_hit_reuses_only_the_authenticated_file_buffer() {
    use cache::LoadEvent::*;
    let store = Store::new();
    let mut cache = store.cache();
    let first = cache
        .compile(3, &small(), limits(), &AtomicBool::new(false))
        .unwrap();
    assert!(!first.cache_hit);
    assert_eq!(cache.spawn_count(), 1);
    assert_eq!(cache::verdict(&Ok(first)), ("accepted", None));
    assert_eq!(
        cache.take_events(),
        [
            Read,
            Rejected("unavailable"),
            MacVerified("fresh_harness_output"),
            Deserialize("fresh_harness_output")
        ]
    );
    let second = cache
        .compile(3, &small(), limits(), &AtomicBool::new(false))
        .unwrap();
    assert!(second.cache_hit);
    assert_eq!(cache.spawn_count(), 1);
    assert_eq!(cache::verdict(&Ok(second)), ("accepted", None));
    assert_eq!(
        cache.take_events(),
        [Read, MacVerified("cache_file"), Deserialize("cache_file")]
    );
}

fn rewrite_manifest(entry: &mut Vec<u8>, change: impl FnOnce(&mut serde_json::Value)) {
    let size = u32::from_be_bytes(entry[..4].try_into().unwrap()) as usize;
    let payload = entry[4 + size..].to_vec();
    let mut manifest: serde_json::Value = serde_json::from_slice(&entry[4..4 + size]).unwrap();
    change(&mut manifest);
    let header = serde_json::to_vec(&manifest).unwrap();
    entry.clear();
    entry.extend_from_slice(&(header.len() as u32).to_be_bytes());
    entry.extend_from_slice(&header);
    entry.extend_from_slice(&payload);
}

#[test]
fn untrusted_entries_are_misses_before_any_native_load() {
    use cache::LoadEvent::*;
    for (case, reason) in [
        ("flipped", "integrity"),
        ("truncated", "integrity"),
        ("missing_manifest", "manifest"),
        ("missing_mac", "manifest"),
        ("wrong_mac", "mac"),
        ("rehashed_tampering", "mac"),
        ("restart", "mac"),
        ("wrong_engine", "provenance"),
        ("wrong_config", "provenance"),
        ("wrong_module", "provenance"),
        ("foreign_key", "key"),
        ("tenant_file", "manifest"),
        ("symlink", "unavailable"),
        ("directory", "not_bounded_regular_file"),
        ("fifo", "not_bounded_regular_file"),
        ("over_cap", "not_bounded_regular_file"),
        ("empty_output", "output_size"),
    ] {
        let store = Store::new();
        let mut cache = store.cache();
        let first = cache
            .compile(3, &small(), limits(), &AtomicBool::new(false))
            .unwrap();
        let path = store.0.join(format!("{}.cache", first.key));
        let mut entry = std::fs::read(&path).unwrap();
        match case {
            "flipped" => *entry.last_mut().unwrap() ^= 1,
            "truncated" => entry.truncate(entry.len() / 2),
            "empty_output" => {
                let size = u32::from_be_bytes(entry[..4].try_into().unwrap()) as usize;
                entry.truncate(4 + size);
            }
            "over_cap" => std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .unwrap()
                .set_len((artifact::supervisor::MAX_COMPILED_OUTPUT_BYTES + 16 * 1024 + 5) as u64)
                .unwrap(),
            "missing_manifest" | "tenant_file" => {
                let size = u32::from_be_bytes(entry[..4].try_into().unwrap()) as usize;
                entry.drain(..4 + size);
            }
            "missing_mac" => rewrite_manifest(&mut entry, |m| {
                m.as_object_mut().unwrap().remove("mac");
            }),
            "wrong_mac" => rewrite_manifest(&mut entry, |m| {
                m["mac"][0] = serde_json::json!(m["mac"][0].as_u64().unwrap() ^ 1)
            }),
            "rehashed_tampering" => {
                use sha2::{Digest, Sha256};
                *entry.last_mut().unwrap() ^= 1;
                let size = u32::from_be_bytes(entry[..4].try_into().unwrap()) as usize;
                let hash: [u8; 32] = Sha256::digest(&entry[4 + size..]).into();
                rewrite_manifest(&mut entry, |m| m["bytes_sha256"] = serde_json::json!(hash));
            }
            "restart" => cache = store.cache(),
            "wrong_engine" => rewrite_manifest(&mut entry, |m| {
                m["provenance"]["engine_version"] = "49.0.6".into()
            }),
            "wrong_config" => rewrite_manifest(&mut entry, |m| {
                m["provenance"]["config_hash"] = serde_json::to_value([0_u8; 32]).unwrap()
            }),
            "wrong_module" => rewrite_manifest(&mut entry, |m| {
                m["provenance"]["module_hash"] = serde_json::to_value([0_u8; 32]).unwrap()
            }),
            "foreign_key" => {
                // A complete authenticated entry produced for a different artifact.
                let other = wat::parse_str("(component)").unwrap();
                let loaded = cache
                    .compile(3, &other, limits(), &AtomicBool::new(false))
                    .unwrap();
                entry = std::fs::read(store.0.join(format!("{}.cache", loaded.key))).unwrap();
            }
            "symlink" | "directory" | "fifo" => {
                std::fs::remove_file(&path).unwrap();
                match case {
                    "symlink" => {
                        let target = store.0.join("tenant-native-file");
                        std::fs::write(&target, &entry).unwrap();
                        std::os::unix::fs::symlink(target, &path).unwrap();
                    }
                    "directory" => std::fs::create_dir(&path).unwrap(),
                    _ => rustix::fs::mkfifoat(
                        rustix::fs::CWD,
                        &path,
                        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
                    )
                    .unwrap(),
                }
            }
            _ => unreachable!(),
        }
        if !["restart", "symlink", "directory", "fifo", "over_cap"].contains(&case) {
            std::fs::write(&path, entry).unwrap();
        }
        cache.take_events();
        let before = cache.spawn_count();
        // A directory cannot be atomically replaced by a regular file. It must still
        // be rejected and freshly compiled, then fail closed at publication.
        let result = cache.compile(3, &small(), limits(), &AtomicBool::new(false));
        if case == "directory" {
            assert_eq!(result.unwrap_err().code, "trusted_cache_unavailable");
        } else {
            assert!(!result.unwrap().cache_hit, "{case}");
        }
        assert_eq!(cache.spawn_count(), before + 1, "{case}");
        assert_eq!(
            cache.take_events(),
            [
                Read,
                Rejected(reason),
                MacVerified("fresh_harness_output"),
                Deserialize("fresh_harness_output")
            ],
            "{case}"
        );
        assert!(cache.last_child_status().unwrap().unwrap().success());
        assert!(std::fs::read_dir(&store.0).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
    }
}

#[test]
fn identity_is_stable_and_changes_for_every_contract_input() {
    use artifact::cache_key::CacheKey;
    let original = CacheKey::new([1; 32], "49.0.5", "x86_64", "fuel;epoch", "0.1.0").unwrap();
    assert_eq!(
        original,
        CacheKey::new([1; 32], "49.0.5", "x86_64", "fuel;epoch", "0.1.0").unwrap()
    );
    for (digest, engine, target, config, interface) in [
        ([2; 32], "49.0.5", "x86_64", "fuel;epoch", "0.1.0"),
        ([1; 32], "49.0.6", "x86_64", "fuel;epoch", "0.1.0"),
        ([1; 32], "49.0.5", "aarch64", "fuel;epoch", "0.1.0"),
        ([1; 32], "49.0.5", "x86_64", "epoch", "0.1.0"),
        ([1; 32], "49.0.5", "x86_64", "fuel;epoch", "0.2.0"),
    ] {
        assert_ne!(
            original,
            CacheKey::new(digest, engine, target, config, interface).unwrap()
        );
    }
}

#[test]
fn qualification_binary_fixtures_match_the_independent_wat_oracle() {
    assert_eq!(fixtures::small(), wat::parse_str("(component)").unwrap());
    let body = "i32.const 3 i32.mul i32.const 1 i32.add\n".repeat(8192);
    let function =
        include_str!("fixtures/was-02/heavy-function.wat").replace(";; OPERATIONS", &body);
    let cpu_wat = include_str!("fixtures/was-02/cpu-heavy.wat")
        .replace(";; FUNCTIONS", &function.repeat(128));
    assert_eq!(fixtures::cpu(), wat::parse_str(cpu_wat).unwrap());
    let memory_wat = include_str!("fixtures/was-02/memory-heavy.wat").replace(
        ";; OPERATIONS",
        &"i32.const 3 i32.mul i32.const 1 i32.add\n".repeat(131072),
    );
    assert_eq!(fixtures::memory(), wat::parse_str(memory_wat).unwrap());
}

#[test]
fn version_three_rejections_stop_before_spawn_and_load() {
    use wasmtime as engine;
    let store = Store::new();
    let mut cache = store.cache();
    let precompiled = engine::Engine::default()
        .precompile_component(&small())
        .unwrap();
    for (version, bytes, diagnostic) in [
        (3, precompiled.as_slice(), "precompiled_rejected"),
        (
            3,
            include_bytes!("fixtures/was-02/native.elf").as_slice(),
            "precompiled_rejected",
        ),
        (
            3,
            include_bytes!("fixtures/was-02/invalid.wasm").as_slice(),
            "invalid_module",
        ),
        (2, small().as_slice(), "unsupported_feature"),
    ] {
        let result = cache.compile(version, bytes, limits(), &AtomicBool::new(false));
        assert_eq!(cache::verdict(&result), ("rejected", Some(diagnostic)));
        assert_eq!(cache.spawn_count(), 0);
        assert!(cache.take_events().is_empty());
        assert_eq!(std::fs::read_dir(&store.0).unwrap().count(), 0);
    }
}

struct ChildFixture(std::path::PathBuf);

impl ChildFixture {
    fn new(directory: &std::path::Path, script: &str) -> Self {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let path = directory.join("trusted-test-child");
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o700)
            .open(&path)
            .unwrap();
        writeln!(
            file,
            "#!/bin/sh\nwhile IFS= read -r line; do :; done\n{script}"
        )
        .unwrap();
        Self(path)
    }
}

#[test]
fn unsuccessful_children_publish_nothing_and_are_reaped() {
    for (script, diagnostic, deadline) in [
        ("exit 0", "compile_failed", 300),
        ("exit 77", "compile_failed", 300),
        ("printf partial; exit 65", "compile_failed", 300),
        ("exec head -c 67108865 /dev/zero", "compile_failed", 300),
        ("exec sleep 60", "compile_deadline_exceeded", 1),
    ] {
        let store = Store::new();
        let fixture_dir = Store::new();
        let child = ChildFixture::new(&fixture_dir.0, script);
        let mut cache = cache::Cache::new(&store.0, &child.0).unwrap();
        let mut budget = limits();
        budget.wall_clock_deadline = Instant::now() + Duration::from_secs(deadline);
        let result = cache.compile(3, &small(), budget, &AtomicBool::new(false));
        assert_eq!(
            cache::verdict(&result),
            ("rejected", Some(diagnostic)),
            "{script}"
        );
        assert_eq!(cache.spawn_count(), 1);
        assert!(cache.last_child_status().unwrap().is_some());
        assert_eq!(
            cache.take_events(),
            [
                cache::LoadEvent::Read,
                cache::LoadEvent::Rejected("unavailable")
            ]
        );
        assert_eq!(std::fs::read_dir(&store.0).unwrap().count(), 0);
    }
}

#[test]
fn cancellation_during_stdout_read_reaps_without_a_partial_entry() {
    let store = Store::new();
    let fixture_dir = Store::new();
    let child = ChildFixture::new(&fixture_dir.0, "exec sleep 60");
    let mut cache = cache::Cache::new(&store.0, &child.0).unwrap();
    let cancelled = AtomicBool::new(false);
    let spawned = cache.spawn_counter();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(30);
            while spawned.load(std::sync::atomic::Ordering::Acquire) == 0 {
                assert!(Instant::now() < deadline);
                std::thread::yield_now();
            }
            cancelled.store(true, std::sync::atomic::Ordering::Release);
        });
        let result = cache.compile(3, &small(), limits(), &cancelled);
        assert_eq!(
            cache::verdict(&result),
            ("rejected", Some("compile_cancelled"))
        );
    });
    assert!(cache.last_child_status().unwrap().is_some());
    assert_eq!(std::fs::read_dir(&store.0).unwrap().count(), 0);
    assert_eq!(
        cache.take_events(),
        [
            cache::LoadEvent::Read,
            cache::LoadEvent::Rejected("unavailable")
        ]
    );
}

fn limit_and_control(bytes: &[u8], failure: CompileLimits, diagnostic: &'static str) {
    let store = Store::new();
    let mut cache = store.cache();
    let result = cache.compile(3, bytes, failure, &AtomicBool::new(false));
    assert_eq!(cache::verdict(&result), ("rejected", Some(diagnostic)));
    assert_eq!(cache.spawn_count(), 1);
    assert!(cache.last_child_status().unwrap().is_some());
    assert_eq!(std::fs::read_dir(&store.0).unwrap().count(), 0);
    assert_eq!(
        cache.take_events(),
        [
            cache::LoadEvent::Read,
            cache::LoadEvent::Rejected("unavailable")
        ]
    );
    let control = cache.compile(3, bytes, limits(), &AtomicBool::new(false));
    assert_eq!(cache::verdict(&control), ("accepted", None));
    assert!(!control.unwrap().cache_hit);
    assert_eq!(cache.spawn_count(), 2);
    assert!(cache.last_child_status().unwrap().unwrap().success());
    assert_eq!(std::fs::read_dir(&store.0).unwrap().count(), 1);
}

#[test]
fn cpu_fixture_fails_only_cpu_and_compiles_at_the_control_budgets() {
    let bytes = fixtures::cpu();
    let mut failure = limits();
    failure.cpu_seconds = 1;
    failure.memory_bytes = 1024 * 1024 * 1024;
    limit_and_control(&bytes, failure, "compile_cpu_limit_exceeded");
}

#[test]
fn memory_fixture_fails_only_memory_and_compiles_at_the_control_budgets() {
    let bytes = fixtures::memory();
    let mut failure = limits();
    failure.cpu_seconds = 60;
    failure.memory_bytes = 128 * 1024 * 1024;
    limit_and_control(&bytes, failure, "compile_memory_limit_exceeded");
}
