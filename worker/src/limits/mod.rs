//! Versioned worker limits contract (WAS-02 ac3, FR-WAS-03). Every limit is mandatory,
//! nonzero, bounded and never above the ceiling the worker enforced before this
//! contract existed. A violation names the offending field and fails closed.
//!
//! The values here are qualification values, not production defaults: the operator
//! approves production values and tolerances (see the was-02 fixtures README).

use serde_json::{Map, Value};
use std::time::Duration;

/// The only contract version this worker accepts.
pub const CONTRACT_VERSION: u64 = 1;

pub const MAX_BUFFER_BYTES: u64 = 1_048_576;
pub const MAX_GUEST_MEMORY_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_HOST_CALL_DEADLINE_MS: u64 = 30_000;
pub const MAX_GUEST_DEADLINE_MS: u64 = 300_000;
pub const MAX_TERMINATION_BUDGET_MS: u64 = 30_000;
pub const MAX_INTERRUPTION_TOLERANCE_MS: u64 = 30_000;
pub const MAX_COMPILER_CPU_SECONDS: u64 = 120;
pub const MAX_COMPILER_MEMORY_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// One field of the contract, in document order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    CompilerCpuSeconds,
    CompilerMemoryBytes,
    GuestMemoryBytes,
    GuestToHostBufferBytes,
    HostToGuestBufferBytes,
    GuestDeadlineMs,
    HostCallDeadlineMs,
    TerminationBudgetMs,
    InterruptionToleranceMs,
}

impl Field {
    pub const ALL: [Field; 9] = [
        Field::CompilerCpuSeconds,
        Field::CompilerMemoryBytes,
        Field::GuestMemoryBytes,
        Field::GuestToHostBufferBytes,
        Field::HostToGuestBufferBytes,
        Field::GuestDeadlineMs,
        Field::HostCallDeadlineMs,
        Field::TerminationBudgetMs,
        Field::InterruptionToleranceMs,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Field::CompilerCpuSeconds => "compiler_cpu_seconds",
            Field::CompilerMemoryBytes => "compiler_memory_bytes",
            Field::GuestMemoryBytes => "guest_memory_bytes",
            Field::GuestToHostBufferBytes => "guest_to_host_buffer_bytes",
            Field::HostToGuestBufferBytes => "host_to_guest_buffer_bytes",
            Field::GuestDeadlineMs => "guest_deadline_ms",
            Field::HostCallDeadlineMs => "host_call_deadline_ms",
            Field::TerminationBudgetMs => "termination_budget_ms",
            Field::InterruptionToleranceMs => "interruption_tolerance_ms",
        }
    }

    /// The largest accepted value. No ceiling is above a limit enforced before the contract.
    pub fn ceiling(self) -> u64 {
        match self {
            Field::CompilerCpuSeconds => MAX_COMPILER_CPU_SECONDS,
            Field::CompilerMemoryBytes => MAX_COMPILER_MEMORY_BYTES,
            Field::GuestMemoryBytes => MAX_GUEST_MEMORY_BYTES,
            Field::GuestToHostBufferBytes | Field::HostToGuestBufferBytes => MAX_BUFFER_BYTES,
            Field::GuestDeadlineMs => MAX_GUEST_DEADLINE_MS,
            Field::HostCallDeadlineMs => MAX_HOST_CALL_DEADLINE_MS,
            Field::TerminationBudgetMs => MAX_TERMINATION_BUDGET_MS,
            Field::InterruptionToleranceMs => MAX_INTERRUPTION_TOLERANCE_MS,
        }
    }

    /// Qualification value: the pre-contract constant where one existed.
    fn qualification(self) -> u64 {
        match self {
            Field::GuestDeadlineMs => 30_000,
            Field::TerminationBudgetMs => 5_000,
            Field::InterruptionToleranceMs => 2_000,
            other => other.ceiling(),
        }
    }

    fn index(self) -> usize {
        Field::ALL
            .iter()
            .position(|field| *field == self)
            .expect("every field is listed")
    }

    fn check(self, value: u64) -> Result<u64, LimitsError> {
        let field = self.name();
        if value == 0 {
            Err(LimitsError::Zero { field })
        } else if value == u64::MAX {
            Err(LimitsError::Unbounded { field })
        } else if value > self.ceiling() {
            Err(LimitsError::AboveCeiling {
                field,
                ceiling: self.ceiling(),
            })
        } else {
            Ok(value)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LimitsError {
    Malformed,
    UnknownVersion { found: Option<u64> },
    UnknownField { name: String },
    Missing { field: &'static str },
    NotAnInteger { field: &'static str },
    Zero { field: &'static str },
    Unbounded { field: &'static str },
    AboveCeiling { field: &'static str, ceiling: u64 },
}

impl LimitsError {
    /// The contract field the error is about, when it is about one.
    pub fn field(&self) -> Option<&str> {
        match self {
            LimitsError::Malformed => None,
            LimitsError::UnknownVersion { .. } => Some("version"),
            LimitsError::UnknownField { name } => Some(name),
            LimitsError::Missing { field }
            | LimitsError::NotAnInteger { field }
            | LimitsError::Zero { field }
            | LimitsError::Unbounded { field }
            | LimitsError::AboveCeiling { field, .. } => Some(field),
        }
    }
}

impl std::fmt::Display for LimitsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LimitsError::Malformed => write!(formatter, "limits contract is not a JSON object"),
            LimitsError::UnknownVersion { found } => {
                write!(formatter, "version: unsupported contract version {found:?}")
            }
            LimitsError::UnknownField { name } => write!(formatter, "{name}: unknown field"),
            LimitsError::Missing { field } => write!(formatter, "{field}: missing"),
            LimitsError::NotAnInteger { field } => {
                write!(formatter, "{field}: not a nonnegative integer")
            }
            LimitsError::Zero { field } => write!(formatter, "{field}: zero"),
            LimitsError::Unbounded { field } => write!(formatter, "{field}: unbounded"),
            LimitsError::AboveCeiling { field, ceiling } => {
                write!(formatter, "{field}: above the ceiling {ceiling}")
            }
        }
    }
}

impl std::error::Error for LimitsError {}

/// A validated contract. Only validation constructs one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    values: [u64; 9],
}

impl Limits {
    /// Parse a JSON document. JSON, unlike TOML, can express `u64::MAX`, so an
    /// unbounded value is a named error rather than a parser failure.
    pub fn parse(text: &str) -> Result<Self, LimitsError> {
        let Ok(Value::Object(document)) = serde_json::from_str::<Value>(text) else {
            return Err(LimitsError::Malformed);
        };
        match document.get("version") {
            None => return Err(LimitsError::Missing { field: "version" }),
            Some(version) if version.as_u64() == Some(CONTRACT_VERSION) => {}
            Some(version) => {
                return Err(LimitsError::UnknownVersion {
                    found: version.as_u64(),
                });
            }
        }
        if let Some(name) = document
            .keys()
            .find(|key| *key != "version" && !Field::ALL.iter().any(|field| field.name() == *key))
        {
            return Err(LimitsError::UnknownField { name: name.clone() });
        }
        let mut values = [0; 9];
        for field in Field::ALL {
            let value = document
                .get(field.name())
                .ok_or(LimitsError::Missing {
                    field: field.name(),
                })?
                .as_u64()
                .ok_or(LimitsError::NotAnInteger {
                    field: field.name(),
                })?;
            values[field.index()] = field.check(value)?;
        }
        Ok(Self { values })
    }

    /// Qualification values: equal to the pre-contract constants wherever one existed.
    pub fn qualification() -> Self {
        let mut values = [0; 9];
        for field in Field::ALL {
            values[field.index()] = field.qualification();
        }
        Self { values }
    }

    /// A copy with one field replaced, validated like a parsed one.
    pub fn replace(&self, field: Field, value: u64) -> Result<Self, LimitsError> {
        let mut values = self.values;
        values[field.index()] = field.check(value)?;
        Ok(Self { values })
    }

    pub fn to_json(&self) -> Value {
        let mut document = Map::new();
        document.insert("version".into(), CONTRACT_VERSION.into());
        for field in Field::ALL {
            document.insert(field.name().into(), self.get(field).into());
        }
        Value::Object(document)
    }

    pub fn get(&self, field: Field) -> u64 {
        self.values[field.index()]
    }

    pub fn compiler_cpu_seconds(&self) -> u64 {
        self.get(Field::CompilerCpuSeconds)
    }

    pub fn compiler_memory_bytes(&self) -> u64 {
        self.get(Field::CompilerMemoryBytes)
    }

    // Every bounded value is at most 4 GiB, so the casts below never truncate on 64-bit.
    pub fn guest_memory_bytes(&self) -> usize {
        self.get(Field::GuestMemoryBytes) as usize
    }

    pub fn guest_to_host_buffer_bytes(&self) -> usize {
        self.get(Field::GuestToHostBufferBytes) as usize
    }

    pub fn host_to_guest_buffer_bytes(&self) -> usize {
        self.get(Field::HostToGuestBufferBytes) as usize
    }

    pub fn guest_deadline(&self) -> Duration {
        Duration::from_millis(self.get(Field::GuestDeadlineMs))
    }

    pub fn host_call_deadline(&self) -> Duration {
        Duration::from_millis(self.get(Field::HostCallDeadlineMs))
    }

    pub fn termination_budget(&self) -> Duration {
        Duration::from_millis(self.get(Field::TerminationBudgetMs))
    }

    pub fn interruption_tolerance(&self) -> Duration {
        Duration::from_millis(self.get(Field::InterruptionToleranceMs))
    }
}
