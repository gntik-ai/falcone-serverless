//! Explicit versioned budgets. Values are private so only validated contracts
//! (or the fixed compatibility profile) can reach the capability boundary.
use serde_json::Value;

pub const BUFFER_BYTES: usize = 1_048_576;
pub const GUEST_MEMORY_BYTES: usize = 256 * 1024 * 1024;
pub const CALL_CAP_MS: u64 = 30_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
pub enum Field {
    CompilerCpuSeconds,
    CompilerMemoryBytes,
    GuestMemoryBytes,
    GuestToHostBytes,
    HostToGuestBytes,
    GuestDeadlineMs,
    HostCallDeadlineMs,
    TerminationBudgetMs,
}
impl Field {
    pub const ALL: [Self; 8] = [
        Self::CompilerCpuSeconds,
        Self::CompilerMemoryBytes,
        Self::GuestMemoryBytes,
        Self::GuestToHostBytes,
        Self::HostToGuestBytes,
        Self::GuestDeadlineMs,
        Self::HostCallDeadlineMs,
        Self::TerminationBudgetMs,
    ];
    pub const fn name(self) -> &'static str {
        match self {
            Self::CompilerCpuSeconds => "compiler_cpu_seconds",
            Self::CompilerMemoryBytes => "compiler_memory_bytes",
            Self::GuestMemoryBytes => "guest_memory_bytes",
            Self::GuestToHostBytes => "guest_to_host_bytes",
            Self::HostToGuestBytes => "host_to_guest_bytes",
            Self::GuestDeadlineMs => "guest_deadline_ms",
            Self::HostCallDeadlineMs => "host_call_deadline_ms",
            Self::TerminationBudgetMs => "termination_budget_ms",
        }
    }
    /// The largest value a validated limits document or an admitted budget may carry.
    pub const fn ceiling(self) -> u64 {
        match self {
            Self::CompilerCpuSeconds => 120,
            Self::CompilerMemoryBytes => 4 * 1024 * 1024 * 1024,
            Self::GuestMemoryBytes => GUEST_MEMORY_BYTES as u64,
            Self::GuestToHostBytes | Self::HostToGuestBytes => BUFFER_BYTES as u64,
            Self::GuestDeadlineMs | Self::HostCallDeadlineMs | Self::TerminationBudgetMs => {
                CALL_CAP_MS
            }
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidationKind {
    Missing,
    Invalid,
    Zero,
    Unbounded,
    AboveCeiling,
    UnknownVersion,
    UnknownField,
}
#[derive(Debug, Eq, PartialEq)]
pub struct ValidationError {
    pub field: String,
    pub kind: ValidationKind,
}
impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {:?}", self.field, self.kind)
    }
}
impl std::error::Error for ValidationError {}

/// The eight validated fields plus the two per-invocation budgets that no limits document
/// carries: guest fuel and the host-call count. Both are unbounded in the compatibility
/// profile and only an admitted revision budget sets them.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    values: [u64; 8],
    fuel: u64,
    max_host_calls: u64,
}
impl Limits {
    /// Compatibility with the previous host ceilings, not implicit parsing defaults.
    pub const fn compatibility() -> Self {
        Self {
            values: [
                120,
                4 * 1024 * 1024 * 1024,
                GUEST_MEMORY_BYTES as u64,
                BUFFER_BYTES as u64,
                BUFFER_BYTES as u64,
                CALL_CAP_MS,
                CALL_CAP_MS,
                500,
            ],
            fuel: u64::MAX,
            max_host_calls: u64::MAX,
        }
    }
    /// Replaces one field. The caller is trusted server code that has already applied the
    /// field's ceiling or an admitted budget; documents go through `parse`.
    #[must_use]
    pub const fn with(mut self, field: Field, value: u64) -> Self {
        self.values[field as usize] = value;
        self
    }
    /// The store's fuel for one invocation.
    #[must_use]
    pub const fn with_fuel(mut self, fuel: u64) -> Self {
        self.fuel = fuel;
        self
    }
    /// The most guest-to-host calls one invocation may make.
    #[must_use]
    pub const fn with_max_host_calls(mut self, calls: u64) -> Self {
        self.max_host_calls = calls;
        self
    }
    pub const fn fuel(self) -> u64 {
        self.fuel
    }
    pub const fn max_host_calls(self) -> u64 {
        self.max_host_calls
    }
    pub const fn value(self, field: Field) -> u64 {
        self.values[field as usize]
    }
    pub fn duration(self, field: Field) -> std::time::Duration {
        std::time::Duration::from_millis(self.value(field))
    }
    pub fn parse(json: &str) -> Result<Self, ValidationError> {
        let error = |field: &str, kind| ValidationError {
            field: field.into(),
            kind,
        };
        let value: Value =
            serde_json::from_str(json).map_err(|_| error("config", ValidationKind::Invalid))?;
        let object = value
            .as_object()
            .ok_or_else(|| error("config", ValidationKind::Invalid))?;
        let version = object
            .get("version")
            .ok_or_else(|| error("version", ValidationKind::Missing))?;
        if version.as_u64() != Some(1) {
            return Err(error("version", ValidationKind::UnknownVersion));
        }
        for name in object.keys() {
            if name != "version" && !Field::ALL.iter().any(|field| field.name() == name) {
                return Err(error(name, ValidationKind::UnknownField));
            }
        }
        let mut values = [0; 8];
        for field in Field::ALL {
            let raw = object
                .get(field.name())
                .ok_or_else(|| error(field.name(), ValidationKind::Missing))?;
            let number = raw
                .as_u64()
                .ok_or_else(|| error(field.name(), ValidationKind::Invalid))?;
            let kind = if number == 0 {
                Some(ValidationKind::Zero)
            } else if number == u64::MAX {
                Some(ValidationKind::Unbounded)
            } else if number > field.ceiling() {
                Some(ValidationKind::AboveCeiling)
            } else {
                None
            };
            if let Some(kind) = kind {
                return Err(error(field.name(), kind));
            }
            values[field as usize] = number;
        }
        Ok(Self {
            values,
            ..Self::compatibility()
        })
    }
}

/// Stable typed outcomes at the supervised compiler boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompileLimitError {
    Cpu,
    Memory,
    Other,
}
impl CompileLimitError {
    pub fn from_code(code: &str) -> Self {
        match code {
            "compile_cpu_limit_exceeded" => Self::Cpu,
            "compile_memory_limit_exceeded" => Self::Memory,
            _ => Self::Other,
        }
    }
}
