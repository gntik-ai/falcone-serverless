//! Standalone WAS-02 qualification. No worker service or tenant execution.
#[path = "../artifact/mod.rs"]
pub mod artifact;
#[path = "../cache/mod.rs"]
pub mod cache;
mod compile_cache_fixtures;

use artifact::supervisor::CompileLimits;
use cache::Cache;
use compile_cache_fixtures::{limits, small};
use std::path::Path;
use std::sync::atomic::AtomicBool;

fn check(ok: bool, reason: &'static str) -> Result<(), String> {
    if ok { Ok(()) } else { Err(reason.into()) }
}

fn compiled(
    cache: &mut Cache,
    bytes: &[u8],
    limits: CompileLimits,
) -> Result<cache::Loaded, String> {
    cache
        .compile(3, bytes, limits, &AtomicBool::new(false))
        .map_err(|error| error.code.to_owned())
}

fn budget_case(
    root: &Path,
    child: &Path,
    bytes: &[u8],
    failure: CompileLimits,
    diagnostic: &str,
) -> Result<(), String> {
    let mut cache = Cache::new(root, child).map_err(|error| error.code.to_owned())?;
    let result = cache.compile(3, bytes, failure, &AtomicBool::new(false));
    check(
        cache::verdict(&result) == ("rejected", Some(diagnostic)),
        "wrong_resource_outcome",
    )?;
    check(
        std::fs::read_dir(root).map_err(|_| "store_read")?.count() == 0,
        "failure_published_entry",
    )?;
    check(
        cache
            .last_child_status()
            .map_err(|_| "child_status")?
            .is_some(),
        "child_not_reaped",
    )?;
    let control = compiled(&mut cache, bytes, limits())?;
    check(
        !control.cache_hit && cache.spawn_count() == 2,
        "control_not_compiled",
    )
}

fn qualify(root: &Path, child: &Path) -> Result<(), String> {
    let directory = root.join("cache");
    let mut cache = Cache::new(&directory, child).map_err(|error| error.code.to_owned())?;
    let first = compiled(&mut cache, &small(), limits())?;
    check(
        !first.cache_hit && cache.spawn_count() == 1,
        "first_not_compiled",
    )?;
    let hit = compiled(&mut cache, &small(), limits())?;
    check(
        hit.cache_hit && hit.key == first.key && cache.spawn_count() == 1,
        "cache_not_reused",
    )?;
    let path = directory.join(format!("{}.cache", first.key));
    let mut bytes = std::fs::read(&path).map_err(|_| "store_read")?;
    *bytes.last_mut().ok_or("empty_entry")? ^= 1;
    std::fs::write(&path, bytes).map_err(|_| "store_write")?;
    let fresh = compiled(&mut cache, &small(), limits())?;
    check(
        !fresh.cache_hit && cache.spawn_count() == 2,
        "tampered_entry_loaded",
    )?;
    drop(cache);
    let mut cache = Cache::new(&directory, child).map_err(|error| error.code.to_owned())?;
    check(
        !compiled(&mut cache, &small(), limits())?.cache_hit && cache.spawn_count() == 1,
        "restart_entry_loaded",
    )?;
    for (version, input, expected) in [
        (3, compile_cache_fixtures::NATIVE, "precompiled_rejected"),
        (3, compile_cache_fixtures::INVALID, "invalid_module"),
        (2, small().as_slice(), "unsupported_feature"),
    ] {
        let result = cache.compile(version, input, limits(), &AtomicBool::new(false));
        check(
            cache::verdict(&result) == ("rejected", Some(expected)) && cache.spawn_count() == 1,
            "validation_outcome",
        )?;
    }
    let cpu = compile_cache_fixtures::cpu();
    let mut cpu_limits = limits();
    cpu_limits.cpu_seconds = 1;
    cpu_limits.memory_bytes = 1024 * 1024 * 1024;
    budget_case(
        &root.join("cpu"),
        child,
        &cpu,
        cpu_limits,
        "compile_cpu_limit_exceeded",
    )?;
    let memory = compile_cache_fixtures::memory();
    let mut memory_limits = limits();
    memory_limits.cpu_seconds = 60;
    memory_limits.memory_bytes = 128 * 1024 * 1024;
    budget_case(
        &root.join("memory"),
        child,
        &memory,
        memory_limits,
        "compile_memory_limit_exceeded",
    )
}

fn main() -> std::process::ExitCode {
    let root = std::env::temp_dir().join(format!("was02-qualification-{}", std::process::id()));
    let result = std::env::current_exe()
        .map_err(|_| "executable_path".to_owned())
        .and_then(|exe| {
            let child = exe
                .parent()
                .ok_or("executable_directory")?
                .join("artifact-compile-child");
            // Never adopt a pre-existing scratch directory or its entries.
            std::fs::create_dir(&root).map_err(|_| "scratch_create")?;
            let result = qualify(&root, &child);
            let cleanup = std::fs::remove_dir_all(&root).map_err(|_| "scratch_cleanup".to_owned());
            cleanup?;
            result
        });
    match result {
        Ok(()) => {
            println!(
                "{}",
                serde_json::json!({"check":"compile_cache","status":"passed"})
            );
            std::process::ExitCode::SUCCESS
        }
        Err(reason) => {
            println!(
                "{}",
                serde_json::json!({"check":"compile_cache","status":"failed","reason":reason})
            );
            std::process::ExitCode::FAILURE
        }
    }
}
