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
    pub fn same_as(&self, other: &NonEmpty) -> bool {
        self.0 == other.0
    }

    /// The identifier, a bounded string over a safe alphabet; never a secret.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

string_type!(
    /// A bounded non-empty text field.
    Text,
    text
);
impl Text {
    /// Library-authored text, never request data.
    pub fn fixed(value: &str) -> Self {
        Self(value.to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

string_type!(
    /// A string that only has to be non-empty (credential claims).
    NonEmpty,
    |value| !value.is_empty()
);
impl NonEmpty {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
string_type!(
    /// `sha256:` followed by 64 lowercase hex digits.
    Digest,
    digest
);
impl Digest {
    /// The 64 lowercase hex digits without the `sha256:` prefix.
    pub fn hex(&self) -> &str {
        &self.0["sha256:".len()..]
    }
}
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
impl Deadline {
    /// Milliseconds since the Unix epoch, truncating below a millisecond. The text was
    /// validated as a real calendar timestamp when it was parsed.
    pub fn unix_millis(&self) -> u64 {
        let number = |range: std::ops::Range<usize>| self.0[range].parse::<i64>().unwrap_or(0);
        let (year, month, day) = (number(0..4), number(5..7), number(8..10));
        // Days from civil (Howard Hinnant): proleptic Gregorian, valid for every year >= 1.
        let year = if month <= 2 { year - 1 } else { year };
        let era = year.div_euclid(400);
        let year_of_era = year.rem_euclid(400);
        let month_index = (month + 9) % 12;
        let day_of_year = (153 * month_index + 2) / 5 + day - 1;
        let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
        let days = era * 146_097 + day_of_era - 719_468;
        let seconds = days * 86_400 + number(11..13) * 3_600 + number(14..16) * 60 + number(17..19);
        let fraction = self.0[19..self.0.len() - 1]
            .strip_prefix('.')
            .map_or(0, |digits| {
                let millis: String = digits.chars().chain("000".chars()).take(3).collect();
                millis.parse::<i64>().unwrap_or(0)
            });
        u64::try_from(seconds * 1_000 + fraction).unwrap_or(0)
    }
}

/// The largest integer the contract carries: 2^53 - 1, exactly representable in any JSON reader.
const MAX_WHOLE: u64 = 9_007_199_254_740_991;

/// Reads an integral JSON number in `0..=MAX_WHOLE`, as the schema's `integer` type does:
/// `1`, `1.0` and `1e0` are the same integer; fractions, negatives and larger values are not.
fn whole<'de, D: Deserializer<'de>>(input: D) -> Result<u64, D::Error> {
    let number = serde_json::Number::deserialize(input)?;
    let value = match number.as_u64() {
        Some(value) => value,
        None => match number.as_f64() {
            Some(float) if float.fract() == 0.0 && (0.0..=MAX_WHOLE as f64).contains(&float) => {
                float as u64
            }
            _ => return Err(D::Error::custom("invalid value")),
        },
    };
    if value <= MAX_WHOLE {
        Ok(value)
    } else {
        Err(D::Error::custom("invalid value"))
    }
}

/// An integer of at least one and at most 2^53 - 1.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Positive(u64);

impl Positive {
    pub fn get(self) -> u64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for Positive {
    fn deserialize<D: Deserializer<'de>>(input: D) -> Result<Self, D::Error> {
        match whole(input)? {
            0 => Err(D::Error::custom("invalid value")),
            value => Ok(Self(value)),
        }
    }
}
redacted!(Positive);

/// An integer of at least zero and at most 2^53 - 1.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Count(u64);

impl<'de> Deserialize<'de> for Count {
    fn deserialize<D: Deserializer<'de>>(input: D) -> Result<Self, D::Error> {
        whole(input).map(Self)
    }
}
redacted!(Count);

impl From<u16> for Count {
    fn from(value: u16) -> Self {
        Self(u64::from(value))
    }
}

impl Count {
    /// Whether this count is more than `total`.
    pub fn exceeds(self, total: Positive) -> bool {
        self.0 > total.0
    }

    /// Whether this count is an HTTP status in `low..=high`.
    pub fn within(self, low: u64, high: u64) -> bool {
        (low..=high).contains(&self.0)
    }

    pub fn is(self, value: u16) -> bool {
        self.0 == u64::from(value)
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

impl Logs {
    /// Worker-captured lines, clipped to the contract: at most 1000 lines of 8192 characters.
    pub fn clipped(lines: Vec<String>) -> Self {
        Self(
            lines
                .into_iter()
                .take(1000)
                .map(|line| match line.char_indices().nth(8192) {
                    Some((end, _)) => line[..end].to_owned(),
                    None => line,
                })
                .collect(),
        )
    }
}
