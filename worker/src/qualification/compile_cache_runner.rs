//! Standalone WAS-02 qualification of embedded fixtures; no worker service.
#[path = "../artifact/mod.rs"]
pub mod artifact;
#[path = "../cache/mod.rs"]
pub mod cache;
mod capability_http;
mod capability_invocations;
mod capability_support;
mod compile_cache_fixtures;
mod counting_alloc;
mod execute_cases;
mod execute_runner;
mod limits_cases;
mod limits_runner;
pub use falcone_serverless_worker::host;
pub use falcone_serverless_worker::limits;

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
    match std::env::args().nth(1).as_deref() {
        Some("--guest-execute-allocation") => return execute_runner::allocation_main(),
        Some(flag) if flag.starts_with("--guest-execute=") => {
            return execute_runner::run(&flag[16..]);
        }
        Some("--limits-ignore-cancellation") => return limits_runner::unresponsive_child(),
        Some("--termination-fallback") => return limits_runner::termination_main(),
        Some("--limits-cancellation") => return limits_runner::main(),
        Some("binding_drop_probe") => {
            capability_invocations::binding_drop_probe();
            return std::process::ExitCode::SUCCESS;
        }
        Some("thread_local_destructor_probe") => {
            capability_invocations::thread_local_destructor_probe();
            return std::process::ExitCode::SUCCESS;
        }
        Some("retained_storage_probe") => {
            capability_invocations::retained_storage_probe();
            return std::process::ExitCode::SUCCESS;
        }
        Some("allocation_probe") => {
            capability_invocations::allocation_probe();
            return std::process::ExitCode::SUCCESS;
        }
        Some("process_budget_probe") => {
            capability_invocations::process_budget_probe();
            return std::process::ExitCode::SUCCESS;
        }
        Some("--capability-gates") => {
            capability_invocations::guest_table_growth_is_bounded_independently_of_linear_memory();
            capability_invocations::exhausted_execution_handles_do_not_start_untracked_workflows();
            capability_invocations::execution_handles_retain_bounded_workflow_metadata();
            capability_invocations::absent_grants_and_ambient_imports_fail_before_a_guest_start();
            capability_invocations::event_scope_and_oversized_lazy_arguments_are_checked_before_binding();
            capability_invocations::logging_is_ungated_and_record_lists_are_lifted_lazily();
            capability_invocations::structured_logging_redacts_guest_credentials_before_binding();
            capability_invocations::record_aggregate_accepts_the_ceiling_and_stops_at_first_excess(
            );
            capability_invocations::storage_keys_cannot_escape_the_verified_binding_and_buffers_are_bounded();
            capability_invocations::checkpoints_support_read_write_and_progress_through_the_granted_storage_handle();
            capability_invocations::workflows_authorize_targets_and_signal_only_host_issued_handles(
            );
            capability_invocations::ungated_context_progress_and_cancellation_follow_the_world_contract();
            capability_invocations::utf16_decoding_is_bounded_by_the_configured_guest_memory();
            capability_invocations::cancellation_interrupts_a_stalled_binding_without_guest_fuel_or_epoch();
            capability_invocations::allocation_instrumentation_reports_an_allocation_above_the_bound();
            capability_invocations::oversized_utf8_and_byte_list_have_bounded_host_allocation();
            capability_invocations::helper_cap_holds_across_fresh_invocations();
            capability_invocations::helper_permits_cover_binding_destructors_even_during_unwinding(
            );
            capability_invocations::helper_permits_cover_thread_local_destructors();
            capability_invocations::retained_storage_memory_is_bounded();
            capability_invocations::dropped_storage_handles_release_live_capacity();
            capability_http::allow_list_resolution_pinning_and_every_redirect_are_enforced();
            capability_http::redirects_preserve_tls_and_scope_guest_credentials_to_the_origin();
            capability_http::private_dns_and_nat64_answers_and_redirects_never_reach_transport();
            capability_http::http_lists_scalars_fields_and_responses_obey_the_aggregate_ceiling();
            capability_http::socket_rebinding_and_redirects_accept_no_forbidden_connections();
        }
        Some(_) => return std::process::ExitCode::FAILURE,
        None => {}
    }
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
            if std::env::args().nth(1).as_deref() == Some("--capability-gates") {
                println!(
                    "{}",
                    serde_json::json!({"check":"capability_gates","status":"passed"})
                );
            }
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
