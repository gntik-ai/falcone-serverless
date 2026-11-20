//! Dependency replay, exact-argument comparison, core-module shape inspection and status
//! computation for the Go profile qualification. Shared by the static suite (which never
//! instantiates a guest) and the pod runner. A mismatch anywhere makes the status not qualified.
use falcone_serverless_worker::{
    adapters::compat_go::{GoFile, Policy, check_guest},
    host::{Bindings, Context, HostError, Operation, Reply},
};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};
use wasmparser::{Parser, Payload};

/// One platform call as the host delivered it. Every argument the guest controls is kept.
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
/// Reads of a key outside the replayed state are denied.
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

// ---------------------------------------------------------------- core module shape

/// What a built component's core modules contain. The component imports are judged by the
/// worker validator; this is the part a validator does not see: how many linear memories the
/// instantiation allocates and how many `cabi_realloc` functions the modules export.
#[derive(Debug, PartialEq, Eq)]
pub struct Shape {
    pub modules: usize,
    /// Linear memories defined (not imported) by any core module. Host::new allows exactly one.
    pub memories: usize,
    pub cabi_realloc_exports: usize,
}

pub fn core_shape(bytes: &[u8]) -> Result<Shape, String> {
    let mut shape = Shape {
        modules: 0,
        memories: 0,
        cabi_realloc_exports: 0,
    };
    for payload in Parser::new(0).parse_all(bytes) {
        match payload.map_err(|error| format!("not a Wasm binary: {error}"))? {
            Payload::ModuleSection { .. } => shape.modules += 1,
            Payload::MemorySection(section) => shape.memories += section.count() as usize,
            Payload::ExportSection(section) => {
                for export in section {
                    let export = export.map_err(|error| format!("bad export: {error}"))?;
                    if export.name == "cabi_realloc" {
                        shape.cabi_realloc_exports += 1;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(shape)
}

/// A built component fits the profile only with exactly one memory and one `cabi_realloc`.
pub fn shape_ok(shape: &Shape) -> Result<(), String> {
    if shape.memories != 1 {
        return Err(format!(
            "{} linear memories are defined; the host allows exactly one",
            shape.memories
        ));
    }
    if shape.cabi_realloc_exports != 1 {
        return Err(format!(
            "{} cabi_realloc exports; the TinyGo runtime must provide exactly one",
            shape.cabi_realloc_exports
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------- static evidence

/// The evidence the static checks and the pod runner read, relative to the profile directory.
/// A relocated bundle carries exactly these paths; `build-profile.sh` copies the same list.
pub const EVIDENCE_PATHS: &[&str] = &["manifest.toml", "negative"];

/// The profile directory in the build checkout.
pub fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("qualification/profiles/go")
}

/// Where the pod runner reads its evidence: an explicit override, else the `evidence`
/// directory shipped beside the built components. Never the build checkout.
pub fn evidence_root(components: &Path, explicit: Option<PathBuf>) -> PathBuf {
    explicit.unwrap_or_else(|| components.join("evidence"))
}

pub fn manifest_in(dir: &Path) -> toml::Value {
    toml::from_str(&fs::read_to_string(dir.join("manifest.toml")).expect("manifest.toml"))
        .expect("manifest.toml is TOML")
}

/// One diagnostic as `(file, line, column, code, construct)`.
pub type Found = (String, usize, usize, String, String);

/// Check a guest source tree under the manifest's own policy.
pub fn check_tree(manifest: &toml::Value, files: &[(String, String)]) -> Vec<Found> {
    let stdlib: Vec<&str> = manifest["stdlib"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["package"].as_str().unwrap())
        .collect();
    let modules: Vec<&str> = manifest["catalogue"]["modules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["path"].as_str().unwrap())
        .collect();
    let policy = Policy {
        stdlib: &stdlib,
        bindings: manifest["bindings"]["module"].as_str().unwrap(),
        modules: &modules,
    };
    let tree: Vec<_> = files
        .iter()
        .map(|(path, text)| GoFile { path, text })
        .collect();
    check_guest(&tree, &policy)
        .diagnostics()
        .iter()
        .map(|d| {
            (
                d.path.clone(),
                d.line,
                d.column,
                d.code.to_owned(),
                d.construct.to_owned(),
            )
        })
        .collect()
}

/// Every file of a fixture directory, as `(name, text)`.
pub fn read_directory(dir: &Path) -> Vec<(String, String)> {
    let mut files: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            (
                path.file_name().unwrap().to_str().unwrap().to_owned(),
                fs::read_to_string(&path).unwrap(),
            )
        })
        .collect();
    files.sort();
    files
}

/// Every negative fixture yields exactly its documented diagnostics: code, file, line, column
/// and construct, and nothing else.
pub fn negative_check(dir: &Path) -> Check {
    let manifest = manifest_in(dir);
    for fixture in manifest["negative"].as_array().unwrap() {
        let name = fixture["name"].as_str().unwrap();
        let found = check_tree(
            &manifest,
            &read_directory(&dir.join(fixture["path"].as_str().unwrap())),
        );
        let expected: Vec<Found> = fixture["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| {
                (
                    d["file"].as_str().unwrap().to_owned(),
                    d["line"].as_integer().unwrap() as usize,
                    d["column"].as_integer().unwrap() as usize,
                    fixture["code"].as_str().unwrap().to_owned(),
                    d["construct"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        if found != expected {
            return Check::Failed(format!(
                "negative fixture {name} did not yield exactly its documented diagnostics"
            ));
        }
    }
    Check::Passed
}

/// The static evidence the profile status depends on. The pod runner extends it with its own
/// outcomes, so a failed static check can never be hidden by runtime results alone. A
/// directory that lacks the bundle is UNVERIFIED, never a pass.
pub fn static_evidence_in(dir: &Path) -> Vec<(String, Check)> {
    let missing: Vec<&str> = EVIDENCE_PATHS
        .iter()
        .copied()
        .filter(|path| !dir.join(path).exists())
        .collect();
    if !missing.is_empty() {
        return vec![(
            "evidence-bundle".into(),
            Check::Unverified(format!(
                "evidence bundle incomplete: {}",
                missing.join(", ")
            )),
        )];
    }
    vec![("negative-fixtures".into(), negative_check(dir))]
}
