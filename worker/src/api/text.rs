//! Request-controlled values. Each type prints `<redacted>` for `Debug` and `Display` and
//! serializes its value unchanged, so only formatted output is redacted.
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Number, Value};
use std::fmt;

const REDACTED: &str = "<redacted>";

macro_rules! redacted {
    ($name:ident) => {
        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(REDACTED)
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(REDACTED)
            }
        }
    };
}

macro_rules! text_type {
    ($(#[$meta:meta])* $name:ident, $valid:expr) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// Build from trusted text; `None` when it breaks the type's rule.
            pub fn new(value: &str) -> Option<Self> {
                Self::try_from(value.to_owned()).ok()
            }
        }
        impl TryFrom<String> for $name {
            type Error = &'static str;
            fn try_from(value: String) -> Result<Self, Self::Error> {
                let valid: fn(&str) -> bool = $valid;
                if valid(&value) {
                    Ok(Self(value))
                } else {
                    Err("invalid text")
                }
            }
        }
        impl From<$name> for String {
            fn from(value: $name) -> String {
                value.0
            }
        }
        redacted!($name);
    };
}

macro_rules! object_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Map<String, Value>);
        redacted!($name);
    };
}

text_type!(
    /// A non-empty identifier or version string supplied by the request.
    Name,
    |value| !value.is_empty()
);
text_type!(
    /// Free text, such as one log line.
    Text,
    |_| true
);
text_type!(
    /// `sha256:` followed by exactly 64 lowercase hex digits.
    Digest,
    |value| value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
);
text_type!(
    /// A catalog base id: lowercase, no `@`, so it can never carry a version or an alias.
    BaseId,
    |value| {
        let mut chars = value.chars();
        chars.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c))
    }
);
text_type!(
    /// An exact `MAJOR.MINOR.PATCH` version, optionally with `-pre` and `+build` parts. Tags,
    /// ranges and aliases such as `latest` or `default` are not versions.
    BaseVersion,
    exact_version
);

fn exact_version(value: &str) -> bool {
    let (core, rest) = match value.find(['-', '+']) {
        Some(at) => value.split_at(at),
        None => (value, ""),
    };
    let numeric = |part: &str| {
        !part.is_empty()
            && part.bytes().all(|b| b.is_ascii_digit())
            && (part.len() == 1 || !part.starts_with('0'))
    };
    let (pre, build) = match rest.strip_prefix('-') {
        Some(tail) => match tail.split_once('+') {
            Some((pre, build)) => (Some(pre), Some(build)),
            None => (Some(tail), None),
        },
        None => (None, rest.strip_prefix('+')),
    };
    // Dot-separated, non-empty identifiers of `[0-9A-Za-z-]`; a numeric prerelease one has no
    // leading zero. A second `+`, or an empty identifier, is not a version.
    let identifiers = |part: &str| {
        part.split('.')
            .all(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
    };
    let numbers: Vec<_> = core.split('.').collect();
    numbers.len() == 3
        && numbers.iter().all(|part| numeric(part))
        && pre.is_none_or(|pre| {
            identifiers(pre)
                && pre
                    .split('.')
                    .all(|id| !id.bytes().all(|b| b.is_ascii_digit()) || numeric(id))
        })
        && build.is_none_or(identifiers)
}

object_type!(
    /// The legacy `main(params)` request envelope, unchanged.
    Params
);
object_type!(
    /// The legacy handler return value (`{statusCode, headers?, body?}`), unchanged.
    HandlerResult
);
object_type!(
    /// One capability grant (`ctr-capability-grants` v1). The worker does not interpret it
    /// here; the capability host enforces it.
    Grant
);

/// A JSON number that is at least zero (credential `iat` and `exp`, unchanged on output).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "Number")]
pub struct Seconds(Number);

impl Seconds {
    pub(super) fn value(&self) -> f64 {
        self.0.as_f64().unwrap_or(f64::NAN)
    }
}

impl TryFrom<Number> for Seconds {
    type Error = &'static str;
    fn try_from(value: Number) -> Result<Self, Self::Error> {
        if value.as_f64().is_some_and(|v| v >= 0.0) {
            Ok(Self(value))
        } else {
            Err("invalid number")
        }
    }
}

/// An integer of at least one: attempt numbers and budgets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u64")]
pub struct Positive(u64);

impl Positive {
    pub(super) fn get(self) -> u64 {
        self.0
    }
}

impl TryFrom<u64> for Positive {
    type Error = &'static str;
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        if value >= 1 {
            Ok(Self(value))
        } else {
            Err("invalid number")
        }
    }
}

/// Read a member whose contract says "absent", not "null". Pair it with `#[serde(default)]`:
/// absent gives `None`, a value gives `Some`, and `null` is an error because it is not a `T`.
pub(super) fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}
