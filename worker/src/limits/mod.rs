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
    Duplicate,
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

// Named, required fields let serde reject duplicate keys before constructing
// values; a generic JSON object would silently overwrite the earlier value.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: Value,
    compile_cpu_seconds: Value,
    compile_memory_bytes: Value,
    compile_wall_clock_ms: Value,
    compile_input_bytes: Value,
    guest_memory_bytes: Value,
    guest_to_host_bytes: Value,
    host_to_guest_bytes: Value,
    guest_deadline_ms: Value,
    host_call_deadline_ms: Value,
    termination_budget_ms: Value,
}

fn document_error(error: serde_json::Error) -> Error {
    let message = error.to_string();
    for (prefix, kind) in [
        ("missing field `", Invalid::Missing),
        ("unknown field `", Invalid::Unknown),
        ("duplicate field `", Invalid::Duplicate),
    ] {
        if let Some(rest) = message.strip_prefix(prefix)
            && let Some((field, _)) = rest.split_once('`')
        {
            return if field == "version" {
                Error::Version
            } else {
                Error::Field {
                    field: field.into(),
                    kind,
                }
            };
        }
    }
    Error::Document
}

/// Private values ensure only fully validated contracts can reach the host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Limits([u64; 10]);
impl Limits {
    pub fn parse(source: &str) -> Result<Self, Error> {
        if !source.trim_start().starts_with('{') {
            return Err(Error::Document);
        }
        let document: Document = serde_json::from_str(source).map_err(document_error)?;
        if document.version.as_u64() != Some(1) {
            return Err(Error::Version);
        }
        let inputs = [
            document.compile_cpu_seconds,
            document.compile_memory_bytes,
            document.compile_wall_clock_ms,
            document.compile_input_bytes,
            document.guest_memory_bytes,
            document.guest_to_host_bytes,
            document.host_to_guest_bytes,
            document.guest_deadline_ms,
            document.host_call_deadline_ms,
            document.termination_budget_ms,
        ];
        let mut values = [0; 10];
        for (field, input) in Field::ALL.into_iter().zip(inputs) {
            let error = |kind| Error::Field {
                field: field.name().into(),
                kind,
            };
            let value = input.as_u64().ok_or_else(|| error(Invalid::WrongType))?;
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
