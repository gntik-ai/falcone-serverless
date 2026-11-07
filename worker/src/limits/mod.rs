//! Closed execution-limits contract v1. Every value is mandatory; these ceilings
//! are validation bounds, never defaults or a production execution profile.
use serde_json::Value;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
pub enum Field {
    CompileCpuSeconds,
    CompileMemoryBytes,
    CompileWallClockMs,
    CompileInputBytes,
    GuestMemoryBytes,
    GuestToHostBytes,
    HostToGuestBytes,
    GuestDeadlineMs,
    HostCallDeadlineMs,
    TerminationBudgetMs,
}

impl Field {
    pub const ALL: [Self; 10] = [
        Self::CompileCpuSeconds,
        Self::CompileMemoryBytes,
        Self::CompileWallClockMs,
        Self::CompileInputBytes,
        Self::GuestMemoryBytes,
        Self::GuestToHostBytes,
        Self::HostToGuestBytes,
        Self::GuestDeadlineMs,
        Self::HostCallDeadlineMs,
        Self::TerminationBudgetMs,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::CompileCpuSeconds => "compile_cpu_seconds",
            Self::CompileMemoryBytes => "compile_memory_bytes",
            Self::CompileWallClockMs => "compile_wall_clock_ms",
            Self::CompileInputBytes => "compile_input_bytes",
            Self::GuestMemoryBytes => "guest_memory_bytes",
            Self::GuestToHostBytes => "guest_to_host_bytes",
            Self::HostToGuestBytes => "host_to_guest_bytes",
            Self::GuestDeadlineMs => "guest_deadline_ms",
            Self::HostCallDeadlineMs => "host_call_deadline_ms",
            Self::TerminationBudgetMs => "termination_budget_ms",
        }
    }

    pub const fn ceiling(self) -> u64 {
        match self {
            Self::CompileCpuSeconds => 3600,
            Self::CompileMemoryBytes => 64 * 1024 * 1024 * 1024,
            Self::CompileWallClockMs => 3_600_000,
            Self::CompileInputBytes => 64 * 1024 * 1024,
            Self::GuestMemoryBytes => 512 * 1024 * 1024,
            // Upper bounds owned by falcone:host@0.1.0, never raised here.
            Self::GuestToHostBytes | Self::HostToGuestBytes => 1_048_576,
            Self::HostCallDeadlineMs => 30_000,
            Self::GuestDeadlineMs => 300_000,
            Self::TerminationBudgetMs => 30_000,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Invalid {
    Missing,
    WrongType,
    Zero,
    Unbounded,
    AboveCeiling,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Error {
    Document,
    Version,
    Field { field: String, kind: Invalid },
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Document => f.write_str("invalid limits document"),
            Self::Version => f.write_str("unsupported limits version"),
            Self::Field { field, kind } => write!(f, "{field}: {kind:?}"),
        }
    }
}
impl std::error::Error for Error {}

/// Private values ensure only fully validated contracts can reach the host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Limits([u64; 10]);
impl Limits {
    pub fn parse(source: &str) -> Result<Self, Error> {
        let document: Value = serde_json::from_str(source).map_err(|_| Error::Document)?;
        let object = document.as_object().ok_or(Error::Document)?;
        if object.get("version").and_then(Value::as_u64) != Some(1) {
            return Err(Error::Version);
        }
        for name in object.keys() {
            if name != "version" && !Field::ALL.iter().any(|field| field.name() == name) {
                return Err(Error::Field {
                    field: name.clone(),
                    kind: Invalid::Unknown,
                });
            }
        }
        let mut values = [0; 10];
        for field in Field::ALL {
            let error = |kind| Error::Field {
                field: field.name().into(),
                kind,
            };
            let value = object
                .get(field.name())
                .ok_or_else(|| error(Invalid::Missing))?
                .as_u64()
                .ok_or_else(|| error(Invalid::WrongType))?;
            let invalid = match value {
                0 => Some(Invalid::Zero),
                u64::MAX => Some(Invalid::Unbounded),
                value if value > field.ceiling() => Some(Invalid::AboveCeiling),
                _ => None,
            };
            if let Some(kind) = invalid {
                return Err(error(kind));
            }
            values[field as usize] = value;
        }
        Ok(Self(values))
    }

    pub fn get(&self, field: Field) -> u64 {
        self.0[field as usize]
    }
}
