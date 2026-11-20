//! Dependency replay, exact-argument comparison and status computation for the JavaScript
//! profile qualification. Shared by the static suite (which never instantiates a guest) and
//! the pod runner. A mismatch anywhere makes the computed status not qualified.
use falcone_serverless_worker::host::{Bindings, Context, HostError, Operation, Reply};
use serde_json::Value;
use std::{collections::BTreeMap, sync::Mutex};

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
