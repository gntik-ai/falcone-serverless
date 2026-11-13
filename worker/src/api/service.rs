//! Typed service accessors. Credential material never contributes to guest input.
use super::{ExecuteRequest, Malformed, Refusal};
use serde_json::{Value, json};

impl ExecuteRequest {
    pub fn attempt_key(&self) -> (String, String) {
        (
            serde_json::to_value(&self.activation_id)
                .unwrap()
                .as_str()
                .unwrap()
                .into(),
            serde_json::to_value(&self.attempt_id)
                .unwrap()
                .as_str()
                .unwrap()
                .into(),
        )
    }

    pub fn projection_value(&self) -> Value {
        serde_json::to_value(&self.projection).unwrap()
    }

    pub fn params_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(&self.params).unwrap()
    }

    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms
            .map(|value| serde_json::to_value(value).unwrap().as_u64().unwrap())
            .unwrap_or(30000)
    }

    pub fn envelope(
        &self,
        result: Value,
        logs: Vec<String>,
        failure: bool,
    ) -> Result<Value, Malformed> {
        let p = self.projection_value();
        let mut identity = json!({
            "function_revision":p["revision"], "runtime_mode":"wasm",
            "artifact_digest":p["artifact_digest"],
            "wasmtime_version":crate::artifact::cache_key::ENGINE_VERSION,
            "language_profile":p["language_profile"], "interface_version":p["interface_version"]
        });
        if let Some(base) = p.get("base") {
            identity["base_digest"] = base.get("digest").unwrap_or(&base["image_digest"]).clone();
        }
        let mut value = json!({"version":"1", "type":"result", "activation_id":self.activation_id,
            "attempt_id":self.attempt_id, "attempt_number":self.attempt_number(),
            "status":if failure {"failure"} else {"success"}, "result":result,"logs":logs,"identity":identity});
        if failure {
            value["class"] = json!("non_retryable");
        }
        super::parse_result(&serde_json::to_vec(&value).unwrap(), self)?;
        Ok(value)
    }

    pub fn refusal_envelope(
        &self,
        refusal: &Refusal,
        logs: Vec<String>,
    ) -> Result<Value, Malformed> {
        self.envelope(serde_json::to_value(refusal).unwrap(), logs, true)
    }
}

impl ExecuteRequest {
    pub fn execution_limits(&self, worker_ms: u64) -> crate::limits::Limits {
        use crate::limits::{Field, Limits};
        let projection = self.projection_value();
        let defaults = Limits::compatibility();
        let timeout = self
            .timeout_ms()
            .min(projection["budgets"]["timeout_ms"].as_u64().unwrap())
            .min(worker_ms)
            .min(defaults.value(Field::GuestDeadlineMs))
            .max(1);
        let mut limits = json!({"version":1});
        for field in Field::ALL {
            limits[field.name()] = json!(defaults.value(field));
        }
        limits[Field::GuestDeadlineMs.name()] = json!(timeout);
        Limits::parse(&limits.to_string()).unwrap()
    }
}

impl ExecuteRequest {
    pub fn host_context(&self) -> crate::host::Context {
        let value = serde_json::to_value(&self.invocation).unwrap();
        let projection = self.projection_value();
        let mut grants = crate::host::Grants::default();
        // Only the separately validated projection's @0.2.0 scopes configure
        // the WIT host. Credential @0.1.0 grants are never mixed into them.
        for grant in projection["grants"].as_array().unwrap() {
            for (name, target) in [
                ("prefixes", &mut grants.prefixes),
                ("hosts", &mut grants.hosts),
                ("topics", &mut grants.topics),
                ("targets", &mut grants.targets),
            ] {
                if let Some(names) = grant["scope"][name].as_array() {
                    target.extend(names.iter().map(|name| name.as_str().unwrap().to_owned()));
                }
            }
        }
        crate::host::Context {
            grants,
            invocation_key: value["invocation_key"].as_str().unwrap().into(),
            attempt_id: self.attempt_key().1,
            idempotency_key: value["idempotency_key"].as_str().unwrap_or("").into(),
            ..Default::default()
        }
    }
    pub fn credential_for_host(&self) -> super::request::Credential {
        self.credential.clone()
    }
}
