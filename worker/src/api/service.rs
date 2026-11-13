//! What the execution service reads from an admitted request, and the checks it makes before it
//! compiles or instantiates anything: the pinned engine, the admitted budgets, the declared
//! imports. Every refusal here is fixed text; none quotes a request value.

use super::grants::Family;
use super::refusal::{Code, Reason, Refusal};
use super::request::ExecuteRequest;
use crate::limits::{Field, Limits};

/// What this worker can honour, from its runtime configuration.
#[derive(Clone, Copy)]
pub struct WorkerCeilings {
    pub max_memory_bytes: u64,
    pub max_wall_clock_ms: u64,
}

/// The concrete scopes of the revision's grants, for the host's verified context.
#[derive(Default)]
pub struct Scopes {
    pub prefixes: Vec<String>,
    pub hosts: Vec<String>,
    pub topics: Vec<String>,
    pub targets: Vec<String>,
}

impl ExecuteRequest {
    /// The activation and attempt, the key of the running execution.
    pub fn key(&self) -> (&str, &str) {
        (self.activation_id().as_str(), self.attempt_id().as_str())
    }

    /// The raw artifact digest as 64 hex digits: the name of the artifact in the store.
    pub fn artifact_hex(&self) -> &str {
        self.projection.artifact_digest.hex()
    }

    /// The `sha256:` digest of the raw artifact.
    pub fn artifact_digest(&self) -> [u8; 32] {
        let mut digest = [0; 32];
        let hex = self.artifact_hex().as_bytes();
        for (byte, pair) in digest.iter_mut().zip(hex.chunks(2)) {
            let nibble = |digit: u8| match digit {
                b'0'..=b'9' => digit - b'0',
                _ => digit - b'a' + 10,
            };
            *byte = nibble(pair[0]) << 4 | nibble(pair[1]);
        }
        digest
    }

    /// The activation runtime identity reports the Wasmtime this worker embeds. A revision
    /// pinned to another engine is refused, never reported as if it had run on this one.
    pub fn check_engine(&self, pinned: &str) -> Result<(), Refusal> {
        if self.projection.engine_version.as_str() == pinned {
            Ok(())
        } else {
            Err(Refusal::new(
                Code::InvalidRequest,
                Reason::EngineVersionMismatch,
            ))
        }
    }

    /// The invocation limits: the admitted budgets, bounded by what this worker allows.
    ///
    /// - Guest memory is the least of `budgets.memory_bytes`, the worker's `max_memory_bytes`
    ///   and the compatibility ceiling. A budget above the worker's maximum is refused, never
    ///   replaced by a default.
    /// - The guest deadline is the least of the time left until the request's `deadline`, the
    ///   projection's `budgets.timeout_ms` and the worker's wall clock. A host call never
    ///   outlives it.
    /// - Fuel and the host-call count are the admitted `budgets.fuel` and
    ///   `budgets.max_host_calls`.
    ///
    /// Every other field keeps its compatibility value.
    pub fn execution_limits(
        &self,
        worker: WorkerCeilings,
        now_unix_ms: u64,
    ) -> Result<Limits, Refusal> {
        let budgets = &self.projection.budgets;
        if budgets.memory_bytes.get() > worker.max_memory_bytes {
            return Err(Refusal::new(
                Code::InvalidRequest,
                Reason::BudgetAboveWorkerCeiling,
            ));
        }
        let memory = budgets
            .memory_bytes
            .get()
            .min(worker.max_memory_bytes)
            .min(Field::GuestMemoryBytes.ceiling());
        let timeout = self
            .deadline
            .unix_millis()
            .saturating_sub(now_unix_ms)
            .min(budgets.timeout_ms.get())
            .min(worker.max_wall_clock_ms);
        Ok(Limits::compatibility()
            .with(Field::GuestMemoryBytes, memory)
            .with(Field::GuestDeadlineMs, timeout)
            .with(
                Field::HostCallDeadlineMs,
                timeout.min(Field::HostCallDeadlineMs.ceiling()),
            )
            .with_fuel(budgets.fuel.get())
            .with_max_host_calls(budgets.max_host_calls.get()))
    }

    /// Every import of the component must be one the projection declares. Whether the import
    /// is also one the worker allows is checked by artifact validation.
    pub fn check_imports(&self, component: &[String]) -> Result<(), Refusal> {
        let declared = |import: &String| {
            self.projection
                .imports
                .iter()
                .any(|declared| declared.as_str() == import)
        };
        if component.iter().all(declared) {
            Ok(())
        } else {
            Err(Refusal::new(
                Code::UndeclaredImport,
                Reason::ImportNotDeclared,
            ))
        }
    }

    /// The action parameters, as the bytes the guest handler receives.
    pub fn params_json(&self) -> Vec<u8> {
        serde_json::to_vec(&self.params).unwrap_or_default()
    }

    /// Every string of the execution credential, for redaction.
    pub fn credential_strings(&self) -> Vec<&str> {
        self.credential.strings()
    }

    pub fn tenant(&self) -> &str {
        self.credential.tenant.as_str()
    }

    pub fn workspace(&self) -> &str {
        self.credential.workspace.as_str()
    }

    pub fn invocation_key(&self) -> &str {
        self.invocation.invocation_key.as_str()
    }

    pub fn idempotency_key(&self) -> &str {
        self.invocation
            .idempotency_key
            .as_ref()
            .map_or("", |key| key.as_str())
    }

    pub fn attempt(&self) -> &str {
        self.attempt_id().as_str()
    }

    /// The scopes of the projection's grants.
    pub fn scopes(&self) -> Scopes {
        let mut scopes = Scopes::default();
        for grant in &self.projection.grants {
            let (family, names) = grant.scope();
            let target = match family {
                Family::Storage => &mut scopes.prefixes,
                Family::Http => &mut scopes.hosts,
                Family::Event => &mut scopes.topics,
                Family::Workflow => &mut scopes.targets,
            };
            target.extend(names.iter().cloned());
        }
        scopes
    }
}
