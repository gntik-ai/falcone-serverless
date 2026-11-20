//! Dependency replay, exact-argument comparison and status computation for the JavaScript
//! profile qualification. Shared by the static suite (which never instantiates a guest) and
//! the pod runner. A mismatch anywhere makes the computed status not qualified.
use falcone_serverless_worker::{
    adapters::compat::{TreeFile, check_tree},
    host::{Bindings, Context, HostError, Operation, Reply},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, path::PathBuf, sync::Mutex};

/// One platform call as the host delivered it. Every argument the guest controls is kept:
/// storage keys, checkpoint flags, event destinations and payload bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    Read {
        key: String,
        checkpoint: bool,
    },
    Write {
        key: String,
        checkpoint: bool,
        data: Vec<u8>,
    },
    Publish {
        destination: String,
        payload: Vec<u8>,
    },
    /// Any operation the recorded case does not use; it is never allowed to match.
    Other(String),
}

/// Platform bindings that replay recorded dependency state and record every call exactly.
/// Reads of a key outside the replayed state are denied, so dropping the state changes both
/// the calls and the guest's response.
pub struct Replay {
    objects: Mutex<BTreeMap<String, Vec<u8>>>,
    calls: Mutex<Vec<Call>>,
    tenant: String,
    workspace: String,
}

impl Replay {
    pub fn new(state: &BTreeMap<String, Vec<u8>>, tenant: &str, workspace: &str) -> Self {
        Self {
            objects: Mutex::new(state.clone()),
            calls: Mutex::new(Vec::new()),
            tenant: tenant.to_owned(),
            workspace: workspace.to_owned(),
        }
    }
    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }
}

impl Bindings for Replay {
    fn call(&self, context: &Context, operation: Operation) -> Result<Reply, HostError> {
        assert_eq!(context.tenant, self.tenant);
        assert_eq!(context.workspace, self.workspace);
        let call = match &operation {
            Operation::Read { key, checkpoint } => Call::Read {
                key: key.clone(),
                checkpoint: *checkpoint,
            },
            Operation::Write {
                key,
                checkpoint,
                data,
            } => Call::Write {
                key: key.clone(),
                checkpoint: *checkpoint,
                data: data.clone(),
            },
            Operation::Publish {
                destination,
                payload,
            } => Call::Publish {
                destination: destination.clone(),
                payload: payload.clone(),
            },
            // Logging is not a dependency of the qualified examples; it is accepted unrecorded.
            Operation::Log { .. } => return Ok(Reply::Empty),
            other => {
                let name = format!("{other:?}");
                let name = name.split([' ', '(', '{']).next().unwrap_or_default();
                self.calls
                    .lock()
                    .unwrap()
                    .push(Call::Other(name.to_owned()));
                return Err(HostError::Denied);
            }
        };
        self.calls.lock().unwrap().push(call.clone());
        match call {
            Call::Read { key, .. } => self
                .objects
                .lock()
                .unwrap()
                .get(&key)
                .cloned()
                .map(Reply::Data)
                .ok_or(HostError::Denied),
            Call::Write { key, data, .. } => {
                self.objects.lock().unwrap().insert(key, data);
                Ok(Reply::Empty)
            }
            _ => Ok(Reply::Empty),
        }
    }
}

/// A recorded case: the dependency state replayed before the invocation, the params, and the
/// exact calls and response the example must produce.
pub struct Case {
    pub name: String,
    pub params: Value,
    pub state: BTreeMap<String, Vec<u8>>,
    pub calls: Vec<Call>,
    pub response: Value,
}

fn text(value: &Value, field: &str) -> String {
    value[field]
        .as_str()
        .unwrap_or_else(|| panic!("case field {field} must be a string"))
        .to_owned()
}

pub fn load_cases(document: &str) -> Vec<Case> {
    let document: Value = serde_json::from_str(document).expect("cases are JSON");
    document["cases"]
        .as_array()
        .expect("cases array")
        .iter()
        .map(|case| Case {
            name: text(case, "name"),
            params: case["params"].clone(),
            state: case["state"]
                .as_object()
                .expect("state object")
                .iter()
                .map(|(key, value)| (key.clone(), value.as_str().unwrap().as_bytes().to_vec()))
                .collect(),
            calls: case["calls"]
                .as_array()
                .expect("calls array")
                .iter()
                .map(|call| match call["op"].as_str().unwrap() {
                    "read" => Call::Read {
                        key: text(call, "key"),
                        checkpoint: call["checkpoint"].as_bool().unwrap(),
                    },
                    "write" => Call::Write {
                        key: text(call, "key"),
                        checkpoint: call["checkpoint"].as_bool().unwrap(),
                        data: text(call, "data").into_bytes(),
                    },
                    "publish" => Call::Publish {
                        destination: text(call, "destination"),
                        payload: text(call, "payload").into_bytes(),
                    },
                    other => panic!("unknown recorded operation {other}"),
                })
                .collect(),
            response: case["response"].clone(),
        })
        .collect()
}

/// Exact comparison: the same calls in the same order with the same bytes, and the same
/// response value. Nothing is normalized, so any mutated argument is a mismatch.
pub fn compare(case: &Case, calls: &[Call], response: &Value) -> Result<(), String> {
    if calls != case.calls.as_slice() {
        return Err(format!(
            "{}: dependency calls differ from the recorded case",
            case.name
        ));
    }
    if response != &case.response {
        return Err(format!(
            "{}: response differs from the recorded case",
            case.name
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Check {
    Passed,
    Failed(String),
    /// Not run, or not verifiable here. It can never contribute to qualification.
    Unverified(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Status {
    Qualified,
    NotQualified,
    Blocked,
}

/// Qualified only when every check passed. Any failure blocks; anything unverified, or no
/// checks at all, leaves the profile not qualified.
pub fn status(checks: &[Check]) -> Status {
    if checks.iter().any(|check| matches!(check, Check::Failed(_))) {
        Status::Blocked
    } else if checks.is_empty() || checks.iter().any(|check| *check != Check::Passed) {
        Status::NotQualified
    } else {
        Status::Qualified
    }
}

/// The request for one built component: the example's request with the component's real
/// imports and artifact digest, and the recorded params. Grants stay exactly as recorded.
pub fn request_for(base: &Value, digest: &str, imports: &[String], params: &Value) -> Value {
    let mut wire = base.clone();
    wire["projection"]["artifact_digest"] = Value::String(digest.to_owned());
    wire["projection"]["imports"] = Value::from(imports.to_vec());
    wire["params"] = params.clone();
    wire
}

// ---------------------------------------------------------------- static evidence

pub fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("qualification/profiles/javascript")
}

pub fn hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub struct Reference {
    pub entry: String,
    pub files: Vec<(String, String)>,
}

pub fn reference() -> Reference {
    let table: toml::Value = toml::from_str(
        &fs::read_to_string(root().join("legacy/sources.toml")).expect("sources.toml"),
    )
    .expect("sources.toml is TOML");
    Reference {
        entry: table["entry"].as_str().unwrap().to_owned(),
        files: table["file"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| {
                (
                    f["path"].as_str().unwrap().to_owned(),
                    f["sha256"].as_str().unwrap().to_owned(),
                )
            })
            .collect(),
    }
}

/// A present file must hash to its pin; an absent one is UNVERIFIED and never passes.
pub fn verify(bytes: Option<&[u8]>, pinned: &str) -> Check {
    match bytes {
        None => Check::Unverified("file absent".into()),
        Some(bytes) if hex(bytes) == pinned => Check::Passed,
        Some(_) => Check::Failed("sha256 mismatch".into()),
    }
}

pub fn reference_checks() -> Vec<(String, Check)> {
    reference()
        .files
        .iter()
        .map(|(path, pinned)| {
            let bytes = fs::read(root().join("legacy").join(path)).ok();
            (path.clone(), verify(bytes.as_deref(), pinned))
        })
        .collect()
}

/// The reference files that are present and hash to their pin. An absent or altered file is
/// never used as evidence.
pub fn verified_files() -> Vec<(String, String)> {
    let reference = reference();
    reference
        .files
        .iter()
        .filter_map(|(path, pinned)| {
            let bytes = fs::read(root().join("legacy").join(path)).ok()?;
            (hex(&bytes) == *pinned)
                .then(|| String::from_utf8(bytes).ok())
                .flatten()
                .map(|text| (path.clone(), text))
        })
        .collect()
}

pub fn tree_report(files: &[(String, String)], entry: &str) -> Vec<Value> {
    let tree: Vec<_> = files
        .iter()
        .map(|(path, text)| TreeFile { path, text })
        .collect();
    check_tree(&tree, entry, None)
        .diagnostics()
        .iter()
        .map(|d| json!({"code": d.code, "construct": d.construct, "file": d.path, "line": d.line, "column": d.column}))
        .collect()
}

pub fn has(report: &[Value], code: &str, file: &str, line: u64) -> bool {
    report
        .iter()
        .any(|d| d["code"] == code && d["file"] == file && d["line"] == line)
}

fn legacy_tree_check(golden: &Value) -> Check {
    let reference = reference();
    let files = verified_files();
    if files.len() != reference.files.len() {
        return Check::Unverified(format!(
            "{} of {} reference files absent or altered; the complete tree was not checked",
            reference.files.len() - files.len(),
            reference.files.len()
        ));
    }
    let report = tree_report(&files, &reference.entry);
    if report.is_empty() {
        return Check::Failed("the unmodified reference tree must not be supported".into());
    }
    for required in golden["required"].as_array().unwrap() {
        if !has(
            &report,
            required["code"].as_str().unwrap(),
            required["file"].as_str().unwrap(),
            required["line"].as_u64().unwrap(),
        ) {
            return Check::Failed(format!("missing required diagnostic {required}"));
        }
    }
    Check::Passed
}

fn legacy_golden_check(golden: &Value) -> Check {
    let reference = reference();
    let files = verified_files();
    if files.len() != reference.files.len() {
        return Check::Unverified("reference tree incomplete; golden not compared".into());
    }
    if golden["status"].as_str() != Some("recorded") {
        return Check::Unverified("the full-tree golden is pending".into());
    }
    if Value::Array(tree_report(&files, &reference.entry)) == golden["diagnostics"] {
        Check::Passed
    } else {
        Check::Failed("computed diagnostics differ from the recorded golden".into())
    }
}

/// Every negative fixture yields exactly its one documented diagnostic, file and line.
fn negative_check() -> Check {
    let manifest: toml::Value =
        toml::from_str(&fs::read_to_string(root().join("manifest.toml")).unwrap()).unwrap();
    for fixture in manifest["negative"].as_array().unwrap() {
        let dir = root().join(fixture["path"].as_str().unwrap());
        let mut files = Vec::new();
        let mut package = None;
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_str().unwrap().to_owned();
            let text = fs::read_to_string(&path).unwrap();
            if name == "package.json" {
                package = Some(text);
            } else {
                files.push((name, text));
            }
        }
        let tree: Vec<_> = files
            .iter()
            .map(|(path, text)| TreeFile { path, text })
            .collect();
        let found: Vec<_> = check_tree(
            &tree,
            fixture["entry"].as_str().unwrap(),
            package.as_deref(),
        )
        .diagnostics()
        .iter()
        .map(|d| (d.path.clone(), d.code.to_owned(), d.line as i64))
        .collect();
        let expected = (
            fixture["file"].as_str().unwrap().to_owned(),
            fixture["code"].as_str().unwrap().to_owned(),
            fixture["line"].as_integer().unwrap(),
        );
        if found != [expected] {
            return Check::Failed(format!(
                "negative fixture {} did not yield exactly its documented diagnostic",
                fixture["path"].as_str().unwrap()
            ));
        }
    }
    Check::Passed
}

/// Whether the recorded expected outputs of the pure workloads are the operator's recording
/// (`recorded`) or a locally created example (`pending-138`). Only a recording can qualify.
fn workload_provenance() -> Vec<(String, Check)> {
    let manifest: toml::Value =
        toml::from_str(&fs::read_to_string(root().join("manifest.toml")).unwrap()).unwrap();
    manifest["workloads"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w.get("expected").is_some())
        .map(|w| {
            let name = w["name"].as_str().unwrap().to_owned();
            let check = match w["expectation"].as_str() {
                Some("recorded") => Check::Passed,
                _ => Check::Unverified(
                    "expected output is a local example; the #138 recording is pending".into(),
                ),
            };
            (format!("workload-expectation:{name}"), check)
        })
        .collect()
}

/// The static evidence the profile status depends on: the reference files, the diagnostics of
/// the unmodified reference tree, its golden, the negative fixtures and the provenance of the
/// workload expectations. The cluster runner extends this with its own outcomes, so absent
/// reference evidence or a failed static check can never be hidden by runtime results alone.
pub fn static_evidence() -> Vec<(String, Check)> {
    let golden: Value = serde_json::from_str(
        &fs::read_to_string(root().join("legacy/expected-diagnostics.json")).unwrap(),
    )
    .unwrap();
    let mut out: Vec<(String, Check)> = reference_checks()
        .into_iter()
        .map(|(path, check)| (format!("reference-file:{path}"), check))
        .collect();
    out.push(("legacy-tree-diagnostics".into(), legacy_tree_check(&golden)));
    out.push(("legacy-golden".into(), legacy_golden_check(&golden)));
    out.push(("negative-fixtures".into(), negative_check()));
    out.extend(workload_provenance());
    out
}
