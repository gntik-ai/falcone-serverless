//! Operator-only execution of the trusted-image-built Go profile components. It never builds a
//! guest toolchain, and it is the binary the cluster suite runs in a restricted pod. Without
//! the opt-in it reports UNVERIFIED and runs nothing.
//!
//! Each component must validate (only `falcone:host/*@0.2.0` imports, so no WASI), define
//! exactly one linear memory, export exactly one `cabi_realloc`, and then instantiate and run
//! under the unchanged `Host::new` limits, including `memories(1)`, to its recorded output.
#[path = "support/profile_go.rs"]
mod profile;

use falcone_serverless_worker::{
    api,
    artifact::{self, supervisor::CompileLimits},
    cache,
    host::{self, Context, Host},
};
use profile::{
    Call, Case, Check, Replay, Status, compare, core_shape, evidence_root, load_cases, request_for,
    root, shape_ok, static_evidence_in, status,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

const MANIFEST: &str = include_str!("../qualification/profiles/go/manifest.toml");
const EVENTS_REQUEST: &str =
    include_str!("../qualification/profiles/go/examples/events-handle/request.json");
const EVENTS_CASES: &str =
    include_str!("../qualification/profiles/go/examples/events-handle/cases.json");

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

/// One invocation under `Host::new`, whose limits are the unchanged sandbox controls: the
/// store-wide `memories(1)` and the guest memory cap. Nothing is raised or bypassed here.
fn invoke(
    loaded: &cache::Loaded,
    context: Context,
    replay: Arc<Replay>,
    params: &[u8],
) -> Result<Value, String> {
    let mut host = Host::new(context, replay);
    host.set_until(Instant::now() + Duration::from_secs(30));
    let execution = host::execute(loaded, host, params);
    let bytes = execution
        .result
        .map_err(|error| format!("handler failed: {}", error.code))?;
    serde_json::from_slice(&bytes).map_err(|_| "handler result is not JSON".to_owned())
}

/// Validate, then inspect the shape: imports, one memory, one `cabi_realloc`. Returns the
/// validated imports.
fn admit(bytes: &[u8], allowed: &[&str]) -> Result<Vec<String>, String> {
    // Validate before trusted compilation: any WASI or non-Falcone import fails here.
    let validated = artifact::validate::validate(bytes, 64 * 1024 * 1024)
        .map_err(|error| format!("import gate rejected the component: {}", error.code))?;
    if let Some(import) = validated
        .imports
        .iter()
        .find(|i| !allowed.contains(&i.as_str()))
    {
        return Err(format!("import outside the allowed list: {import}"));
    }
    shape_ok(&core_shape(bytes)?)?;
    Ok(validated.imports)
}

#[test]
#[ignore = "UNVERIFIED: requires FS_TOOLCHAIN_CACHE or FS_PROFILE_COMPONENTS and trusted-image-built Go components"]
fn built_profile_components() {
    let components = match (
        std::env::var_os("FS_PROFILE_COMPONENTS"),
        std::env::var_os("FS_TOOLCHAIN_CACHE"),
    ) {
        (Some(components), _) => PathBuf::from(components),
        (None, Some(cache_root)) => PathBuf::from(cache_root).join("profile-go"),
        (None, None) => {
            // Nothing ran, so nothing is qualified: the computed status says so explicitly.
            let directory = std::env::var_os("FS_PROFILE_EVIDENCE")
                .map(PathBuf::from)
                .unwrap_or_else(root);
            let mut checks: Vec<Check> = static_evidence_in(&directory)
                .into_iter()
                .map(|(_, c)| c)
                .collect();
            checks.push(Check::Unverified(
                "FS_TOOLCHAIN_CACHE and FS_PROFILE_COMPONENTS unset".into(),
            ));
            assert_ne!(status(&checks), Status::Qualified);
            println!("UNVERIFIED: no toolchain cache or components; no guest was executed");
            return;
        }
    };
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
    let directory = std::env::temp_dir().join(format!("profile-go-{}", std::process::id()));
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
    let evidence = static_evidence_in(&evidence_directory);
    for (name, check) in &evidence {
        println!("{name}: {check:?}");
    }
    let mut checks: Vec<Check> = evidence.into_iter().map(|(_, c)| c).collect();
    let mut record = |name: &str, outcome: Result<(), String>| {
        println!("{name}: {outcome:?}");
        checks.push(match outcome {
            Ok(()) => Check::Passed,
            Err(reason) => Check::Failed(format!("{name}: {reason}")),
        });
    };

    // A negative fixture is never built, so no component of that name may exist.
    for negative in manifest["negative"].as_array().unwrap() {
        let name = negative["name"].as_str().unwrap();
        record(
            &format!("negative/{name}/not-built"),
            (!components.join(format!("{name}.wasm")).exists())
                .then_some(())
                .ok_or_else(|| "a component exists for a negative fixture".to_owned()),
        );
    }

    // The pure workloads against their recorded outputs.
    for (name, input, expected) in [
        (
            "json-transform",
            include_str!("../qualification/profiles/go/workloads/json-transform/input.json"),
            include_str!("../qualification/profiles/go/workloads/json-transform/expected.json"),
        ),
        (
            "text-scan",
            include_str!("../qualification/profiles/go/workloads/text-scan/input.json"),
            include_str!("../qualification/profiles/go/workloads/text-scan/expected.json"),
        ),
        (
            "hash-sha256",
            include_str!("../qualification/profiles/go/workloads/hash-sha256/input.json"),
            include_str!("../qualification/profiles/go/workloads/hash-sha256/expected.json"),
        ),
    ] {
        let outcome = (|| {
            let bytes = std::fs::read(components.join(format!("{name}.wasm")))
                .map_err(|_| "missing trusted component".to_owned())?;
            let imports = admit(&bytes, &allowed)?;
            let mut params: Value = serde_json::from_str(input).unwrap();
            // The execution request requires a path; the handlers ignore unknown fields.
            params["__ow_path"] = json!("/v1/workspaces/ws-synthetic-0001/openapi");
            let wire = request_for(
                &base,
                &format!("sha256:{:x}", Sha256::digest(&bytes)),
                &imports,
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
            let actual = invoke(&loaded, context, replay, &request.plan().params)?;
            let expected: Value = serde_json::from_str(expected).unwrap();
            (actual == expected)
                .then_some(())
                .ok_or_else(|| "output differs from the recorded expectation".to_owned())
        })();
        record(name, outcome);
    }

    // The events example: replay the recorded state, then compare every dependency argument.
    let events = (|| -> Result<Vec<Case>, String> {
        let bytes = std::fs::read(components.join("events-handle.wasm"))
            .map_err(|_| "missing trusted component".to_owned())?;
        let imports = admit(&bytes, &allowed)?;
        let cases = load_cases(EVENTS_CASES);
        for case in &cases {
            let wire = request_for(
                &base,
                &format!("sha256:{:x}", Sha256::digest(&bytes)),
                &imports,
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
                let response = invoke(&loaded, context, replay.clone(), &request.plan().params);
                (replay.calls(), response)
            };
            // Replayed state in: calls and response must equal the recording exactly.
            let (calls, response) = run(&case.state);
            let outcome = response.and_then(|response| compare(case, &calls, &response));
            record(&format!("events-handle/{}", case.name), outcome);
            // Dropping the replayed state changes what the guest sees, so it must be detected.
            let (calls, response) = run(&BTreeMap::new());
            let dropped = response.and_then(|response| compare(case, &calls, &response));
            record(
                &format!("events-handle/{}/dropped-state-is-detected", case.name),
                dropped
                    .is_err()
                    .then_some(())
                    .ok_or("dropping the state was not detected".to_owned()),
            );
            // A changed key, destination, payload or response changes the recording.
            let (calls, response) = run(&case.state);
            let response = response.unwrap_or(Value::Null);
            for (what, mutated) in mutated_cases(case) {
                record(
                    &format!("events-handle/{}/{what}-is-detected", case.name),
                    compare(&mutated, &calls, &response)
                        .is_err()
                        .then_some(())
                        .ok_or("the mutation was not detected".to_owned()),
                );
            }
        }
        Ok(cases)
    })();
    if let Err(reason) = events {
        record("events-handle", Err(reason));
    }
    drop(compiler);
    drop(scratch);
    assert!(!directory.exists());
    match status(&checks) {
        Status::Qualified => println!("{}", json!({"check":"profile_go","status":"passed"})),
        Status::Blocked => panic!("profile blocked: {checks:?}"),
        // Nothing failed, but evidence is pending: report it and emit no success record.
        Status::NotQualified => {
            for check in &checks {
                if let Check::Unverified(reason) = check {
                    println!("UNVERIFIED: {reason}");
                }
            }
            println!("{}", json!({"check":"profile_go","status":"not-qualified"}));
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
