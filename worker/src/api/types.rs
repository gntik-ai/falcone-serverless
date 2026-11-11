//! Validated scalar newtypes. Each parses with the rule of its schema definition and never
//! echoes the input in an error or in formatted output.

use super::wire::{constant, redacted, unit_debug};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

constant!(Version, "1");
constant!(InterfaceVersion, "0.2.0");

/// Runtime mode of a revision (ADR 0010); a required projection field.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeMode {
    Wasm,
    Oci,
}
unit_debug!(RuntimeMode: Wasm, Oci);

fn string<'de, D: Deserializer<'de>>(
    input: D,
    accept: fn(&str) -> bool,
) -> Result<String, D::Error> {
    let value = String::deserialize(input)?;
    if accept(&value) {
        Ok(value)
    } else {
        Err(D::Error::custom("invalid value"))
    }
}

/// Defines a transparent string newtype checked by `accept`.
macro_rules! string_type {
    ($(#[$doc:meta])* $ty:ident, $accept:expr) => {
        $(#[$doc])*
        #[derive(Clone, PartialEq, Eq, Serialize)]
        #[serde(transparent)]
        pub struct $ty(String);

        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: Deserializer<'de>>(input: D) -> Result<Self, D::Error> {
                string(input, $accept).map(Self)
            }
        }
        redacted!($ty);
    };
}

fn id(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=128).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"._:@/-".contains(b))
}

fn text(value: &str) -> bool {
    (1..=1024).contains(&value.chars().count())
}

fn digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

fn catalog_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=128).contains(&bytes.len())
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(b))
}

/// A concrete `MAJOR.MINOR.PATCH`; an alias such as `default` or `latest` never matches.
fn catalog_version(value: &str) -> bool {
    let parts: Vec<_> = value.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// `YYYY-MM-DDTHH:MM:SS[.f]Z` in UTC with a real calendar date.
fn timestamp(value: &str) -> bool {
    let b = value.as_bytes();
    if b.len() < 20 || b[b.len() - 1] != b'Z' {
        return false;
    }
    let shape = [(4, b'-'), (7, b'-'), (10, b'T'), (13, b':'), (16, b':')];
    let digits = |range: std::ops::Range<usize>| b[range].iter().all(u8::is_ascii_digit);
    if !shape.iter().all(|&(at, c)| b[at] == c)
        || !(digits(0..4) && digits(5..7) && digits(8..10))
        || !(digits(11..13) && digits(14..16) && digits(17..19))
    {
        return false;
    }
    let fraction = &b[19..b.len() - 1];
    let fraction_ok = fraction.is_empty()
        || (fraction[0] == b'.'
            && (2..=10).contains(&fraction.len())
            && fraction[1..].iter().all(u8::is_ascii_digit));
    let number = |range: std::ops::Range<usize>| value[range].parse::<u32>().unwrap_or(u32::MAX);
    let (year, month, day) = (number(0..4), number(5..7), number(8..10));
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => 0,
    };
    fraction_ok
        && (1..=days).contains(&day)
        && number(11..13) < 24
        && number(14..16) < 60
        && number(17..19) < 60
}

string_type!(
    /// An activation, attempt, revision or invocation identifier.
    Id,
    id
);
impl Id {
    pub(crate) fn same_as(&self, other: &NonEmpty) -> bool {
        self.0 == other.0
    }
}

string_type!(
    /// A bounded non-empty text field.
    Text,
    text
);
impl Text {
    /// Library-authored text, never request data.
    pub(crate) fn fixed(value: &str) -> Self {
        Self(value.to_owned())
    }
}

string_type!(
    /// A string that only has to be non-empty (credential claims).
    NonEmpty,
    |value| !value.is_empty()
);
string_type!(
    /// `sha256:` followed by 64 lowercase hex digits.
    Digest,
    digest
);
string_type!(
    /// A catalog base name; the concrete part of `id@version`.
    CatalogId,
    catalog_id
);
string_type!(
    /// A concrete catalog base version, never an unresolved default alias.
    CatalogVersion,
    catalog_version
);
string_type!(
    /// An absolute UTC deadline.
    Deadline,
    timestamp
);

/// An integer of at least one.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Positive(u64);

impl<'de> Deserialize<'de> for Positive {
    fn deserialize<D: Deserializer<'de>>(input: D) -> Result<Self, D::Error> {
        match u64::deserialize(input)? {
            0 => Err(D::Error::custom("invalid value")),
            value => Ok(Self(value)),
        }
    }
}
redacted!(Positive);

impl Positive {
    /// Whether `completed` is more than `total`.
    pub(crate) fn exceeds(completed: u64, total: Self) -> bool {
        completed > total.0
    }
}

/// Seconds since the epoch in a credential; a non-negative JSON number.
#[derive(Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Seconds(serde_json::Number);

impl<'de> Deserialize<'de> for Seconds {
    fn deserialize<D: Deserializer<'de>>(input: D) -> Result<Self, D::Error> {
        let number = serde_json::Number::deserialize(input)?;
        if number.as_f64().is_some_and(|value| value >= 0.0) {
            Ok(Self(number))
        } else {
            Err(D::Error::custom("invalid value"))
        }
    }
}
redacted!(Seconds);

/// Free text bounded by a character count, for log lines.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Logs(Vec<String>);

impl<'de> Deserialize<'de> for Logs {
    fn deserialize<D: Deserializer<'de>>(input: D) -> Result<Self, D::Error> {
        let lines = Vec::<String>::deserialize(input)?;
        if lines.len() <= 1000 && lines.iter().all(|l| l.chars().count() <= 8192) {
            Ok(Self(lines))
        } else {
            Err(D::Error::custom("invalid value"))
        }
    }
}
redacted!(Logs);
