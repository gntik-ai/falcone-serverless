//! Operator-only execution of the trusted-image-built JavaScript profile components. It never
//! builds a guest toolchain, and it is the binary the cluster suite runs in a restricted pod.
//! Without the opt-in it reports UNVERIFIED and runs nothing.
#[path = "support/profile_javascript.rs"]
mod profile;

use falcone_serverless_worker::{
    api,
    artifact::{self, supervisor::CompileLimits},
    cache,
    host::{self, Context},
};
use profile::{
    Call, Case, Check, Replay, Status, compare, evidence_root, load_cases, request_for, root,
    static_evidence_in, status,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

const MANIFEST: &str = include_str!("../qualification/profiles/javascript/manifest.toml");
const EVENTS_REQUEST: &str =
    include_str!("../qualification/profiles/javascript/examples/events-handle/request.json");
const EVENTS_CASES: &str =
    include_str!("../qualification/profiles/javascript/examples/events-handle/cases.json");

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
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

/// One invocation: the params as raw JSON bytes (the handler's `params`), under the recorded
/// grants, with the case's dependency state replayed into the bindings before the call.
fn invoke(
    loaded: &cache::Loaded,
    request: &api::ExecuteRequest,
    context: Context,
    replay: Arc<Replay>,
) -> Result<Value, String> {
    let limits = request
        .execution_limits(api::Ceilings {
            max_memory_bytes: 64 * 1024 * 1024,
            max_wall_clock_ms: 30_000,
        })
        .unwrap();
    let mut host = host::Host::with_limits(context, replay, limits.limits);
    host.set_fuel(limits.fuel);
    host.set_max_host_calls(limits.max_host_calls);
    host.set_until(Instant::now() + limits.timeout);
    let execution = host::execute(loaded, host, &request.plan().params);
    let bytes = execution
        .result
        .map_err(|error| format!("handler failed: {}", error.code))?;
    serde_json::from_slice(&bytes).map_err(|_| "handler result is not JSON".to_owned())
}

#[test]
#[ignore = "UNVERIFIED: requires FS_TOOLCHAIN_CACHE and trusted-image-built JS components"]
fn built_profile_components() {
    let Some(cache_root) = std::env::var_os("FS_TOOLCHAIN_CACHE") else {
        // Nothing ran, so nothing is qualified: the computed status says so explicitly.
        // Not running from a bundle: the build checkout is the evidence.
        let directory = std::env::var_os("FS_PROFILE_EVIDENCE")
            .map(PathBuf::from)
            .unwrap_or_else(root);
        let mut checks: Vec<Check> = static_evidence_in(&directory)
            .into_iter()
            .map(|(_, c)| c)
            .collect();
        checks.push(Check::Unverified("FS_TOOLCHAIN_CACHE unset".into()));
        assert_ne!(status(&checks), Status::Qualified);
        println!("UNVERIFIED: FS_TOOLCHAIN_CACHE unset; no guest toolchain or component executed");
        return;
    };
    let components = std::env::var_os("FS_PROFILE_COMPONENTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(cache_root).join("profile"));
    // The evidence travels with the components (or FS_PROFILE_EVIDENCE); the build checkout is
    // not available in the pod and is never read here.
    let evidence_directory = evidence_root(
        &components,
        std::env::var_os("FS_PROFILE_EVIDENCE").map(PathBuf::from),
    );
    let exe = std::env::current_exe().unwrap();
    let parent = exe.parent().unwrap();
    let binaries = if parent.file_name().is_some_and(|name| name == "deps") {
        parent.parent().unwrap()
    } else {
        parent
    };
    let directory = std::env::temp_dir().join(format!("profile-js-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let scratch = Scratch(directory.clone());
    let mut compiler =
        cache::Cache::new(&directory, binaries.join("artifact-compile-child")).unwrap();
    let manifest: toml::Value = toml::from_str(MANIFEST).unwrap();
    let allowed: Vec<&str> = manifest["wit"]["interfaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry.as_str().unwrap())
        .collect();
    let base: Value = serde_json::from_str(EVENTS_REQUEST).unwrap();
    // The status is computed over the static evidence (reference files, tree diagnostics,
    // golden, negative fixtures, expectation provenance) together with the runtime outcomes,
    // so runtime results alone can never qualify and a static failure always blocks.
    let evidence = static_evidence_in(&evidence_directory);
    for (name, check) in &evidence {
        println!("{name}: {check:?}");
    }
    let mut checks: Vec<Check> = evidence.into_iter().map(|(_, c)| c).collect();
    // `pending`: the expectation is only a local example, so a match is UNVERIFIED, not a pass.
    let mut record = |name: &str, outcome: Result<(), String>, pending: bool| {
        println!("{name}: {outcome:?}");
        checks.push(match outcome {
            Ok(()) if pending => Check::Unverified(format!(
                "{name}: matched a local example, not the #138 recording"
            )),
            Ok(()) => Check::Passed,
            Err(reason) => Check::Failed(format!("{name}: {reason}")),
        });
    };

    // The pure workloads against their expected outputs. While the expectation is only a local
    // example (pending the #138 recording) a match is UNVERIFIED, never a pass; a mismatch fails.
    let recorded = |name: &str| {
        manifest["workloads"].as_array().unwrap().iter().any(|w| {
            w["name"].as_str() == Some(name) && w["expectation"].as_str() == Some("recorded")
        })
    };
    for (name, input, expected) in [
        (
            "json-transform",
            include_str!(
                "../qualification/profiles/javascript/workloads/json-transform/input.json"
            ),
            include_str!(
                "../qualification/profiles/javascript/workloads/json-transform/expected.json"
            ),
        ),
        (
            "text-regex",
            include_str!("../qualification/profiles/javascript/workloads/text-regex/input.json"),
            include_str!("../qualification/profiles/javascript/workloads/text-regex/expected.json"),
        ),
        (
            "hash-sha256",
            include_str!("../qualification/profiles/javascript/workloads/hash-sha256/input.json"),
            include_str!(
                "../qualification/profiles/javascript/workloads/hash-sha256/expected.json"
            ),
        ),
    ] {
        let outcome = (|| {
            let bytes = std::fs::read(components.join(format!("{name}.wasm")))
                .map_err(|_| "missing trusted component".to_owned())?;
            // Validate before trusted compilation: any WASI or non-Falcone import fails here.
            let validated = artifact::validate::validate(&bytes, 64 * 1024 * 1024)
                .map_err(|_| "import gate rejected the component".to_owned())?;
            if let Some(import) = validated
                .imports
                .iter()
                .find(|i| !allowed.contains(&i.as_str()))
            {
                return Err(format!("import outside the allowed list: {import}"));
            }
            let mut params: Value = serde_json::from_str(input).unwrap();
            params["__ow_path"] = json!("/v1/workspaces/ws-synthetic-0001/openapi");
            let wire = request_for(
                &base,
                &format!("sha256:{:x}", Sha256::digest(&bytes)),
                &validated.imports,
                &params,
            );
            let request = api::parse_request(&serde_json::to_vec(&wire).unwrap())
                .map_err(|_| "request rejected".to_owned())?;
            let loaded = compile(&mut compiler, &bytes, &request);
            let context = request.plan().context;
            let replay = Arc::new(Replay::new(
                &BTreeMap::new(),
                &context.tenant,
                &context.workspace,
            ));
            let actual = invoke(&loaded, &request, context, replay)?;
            let expected: Value = serde_json::from_str(expected).unwrap();
            (actual == expected)
                .then_some(())
                .ok_or_else(|| "output differs from the recorded expectation".to_owned())
        })();
        record(name, outcome, !recorded(name));
    }

    // The events example: replay the recorded state, then compare every dependency argument.
    let events = (|| -> Result<Vec<Case>, String> {
        let bytes = std::fs::read(components.join("events-handle.wasm"))
            .map_err(|_| "missing trusted component".to_owned())?;
        let validated = artifact::validate::validate(&bytes, 64 * 1024 * 1024)
            .map_err(|_| "import gate rejected the component".to_owned())?;
        if let Some(import) = validated
            .imports
            .iter()
            .find(|i| !allowed.contains(&i.as_str()))
        {
            return Err(format!("import outside the allowed list: {import}"));
        }
        let cases = load_cases(EVENTS_CASES);
        for case in &cases {
            let wire = request_for(
                &base,
                &format!("sha256:{:x}", Sha256::digest(&bytes)),
                &validated.imports,
                &case.params,
            );
            let request =
                api::parse_request(&serde_json::to_vec(&wire).unwrap()).map_err(|_| {
                    "request rejected: the grants must admit the component's imports".to_owned()
                })?;
            let loaded = compile(&mut compiler, &bytes, &request);
            let run = |state: &BTreeMap<String, Vec<u8>>| {
                let context = request.plan().context;
                let replay = Arc::new(Replay::new(state, &context.tenant, &context.workspace));
                let response = invoke(&loaded, &request, context, replay.clone());
                (replay.calls(), response)
            };
            // Replayed state in: calls and response must equal the recording exactly.
            let (calls, response) = run(&case.state);
            let outcome = response.and_then(|response| compare(case, &calls, &response));
            record(&format!("events-handle/{}", case.name), outcome, false);
            // Mutations must be caught. Dropping the replayed state changes what the guest
            // sees; a changed key, destination, payload or response changes the recording.
            let (calls, response) = run(&BTreeMap::new());
            let dropped = response.and_then(|response| compare(case, &calls, &response));
            record(
                &format!("events-handle/{}/dropped-state-is-detected", case.name),
                dropped
                    .is_err()
                    .then_some(())
                    .ok_or("dropping the state was not detected".to_owned()),
                false,
            );
            let (calls, response) = run(&case.state);
            let response = response.unwrap_or(Value::Null);
            for (what, mutated) in mutated_cases(case) {
                record(
                    &format!("events-handle/{}/{what}-is-detected", case.name),
                    compare(&mutated, &calls, &response)
                        .is_err()
                        .then_some(())
                        .ok_or("the mutation was not detected".to_owned()),
                    false,
                );
            }
        }
        Ok(cases)
    })();
    if let Err(reason) = events {
        record("events-handle", Err(reason), false);
    }
    drop(compiler);
    drop(scratch);
    assert!(!directory.exists());
    match status(&checks) {
        Status::Qualified => println!(
            "{}",
            json!({"check":"profile_javascript","status":"passed"})
        ),
        Status::Blocked => panic!("profile blocked: {checks:?}"),
        // Nothing failed, but evidence is pending: report it and emit no success record.
        Status::NotQualified => {
            for check in &checks {
                if let Check::Unverified(reason) = check {
                    println!("UNVERIFIED: {reason}");
                }
            }
            println!(
                "{}",
                json!({"check":"profile_javascript","status":"not-qualified"})
            );
        }
    }
}

/// Copies of a recorded case whose expectation was changed in one way.
fn mutated_cases(case: &Case) -> Vec<(&'static str, Case)> {
    let copy = |calls: Vec<Call>, response: Value| Case {
        name: case.name.clone(),
        params: case.params.clone(),
        state: case.state.clone(),
        calls,
        response,
    };
    let mut out = Vec::new();
    for call in 0..case.calls.len() {
        let mut calls = case.calls.clone();
        let what = match &mut calls[call] {
            Call::Read { key, .. } | Call::Write { key, .. } => {
                key.push('x');
                "changed-key"
            }
            Call::Publish { destination, .. } => {
                destination.push('x');
                "changed-destination"
            }
            Call::Other(_) => continue,
        };
        out.push((what, copy(calls, case.response.clone())));
        let mut calls = case.calls.clone();
        let what = match &mut calls[call] {
            Call::Read { checkpoint, .. } | Call::Write { checkpoint, .. } => {
                *checkpoint = !*checkpoint;
                "flipped-checkpoint-flag"
            }
            Call::Publish { payload, .. } => {
                payload.push(b' ');
                "changed-payload"
            }
            Call::Other(_) => continue,
        };
        out.push((what, copy(calls, case.response.clone())));
    }
    let mut response = case.response.clone();
    response["statusCode"] = json!(500);
    out.push(("changed-response", copy(case.calls.clone(), response)));
    out
}
