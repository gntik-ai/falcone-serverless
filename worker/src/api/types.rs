//! Validated scalar types shared by the v1 execution API.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A rejected v1 value. Messages are fixed text: they never echo the rejected input,
/// which may be an execution credential.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContractError(String);

impl ContractError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    /// Reports a serde failure without echoing any input value. Field-shape messages
    /// and this module's own messages are safe; type and value mismatches quote the
    /// input and are replaced.
    pub fn from_serde(error: &serde_json::Error) -> Self {
        let text = error.to_string();
        let text = text.split(" at line ").next().unwrap_or_default();
        if error.is_data()
            && (text.starts_with("missing field")
                || text.starts_with("unknown field")
                || text.starts_with("unknown variant")
                || text.starts_with("v1: "))
        {
            Self(text.to_owned())
        } else if error.is_data() {
            Self::new("v1: value does not match the schema")
        } else {
            Self::new("v1: not valid JSON")
        }
    }
}

impl fmt::Display for ContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ContractError {}

/// Reads a v1 value and requires it to be canonical: the typed value must serialize back
/// to exactly the input. Optional fields are spelled by omission, so an explicit `null`
/// (read as absent) is rejected here.
pub(super) fn parse<T: serde::de::DeserializeOwned + Serialize>(
    value: &serde_json::Value,
) -> Result<T, ContractError> {
    let parsed: T =
        serde_json::from_value(value.clone()).map_err(|error| ContractError::from_serde(&error))?;
    match serde_json::to_value(&parsed) {
        Ok(canonical) if canonical == *value => Ok(parsed),
        _ => Err(ContractError::new(
            "v1: optional fields are omitted, never null",
        )),
    }
}

/// A non-empty identifier of at most 256 bytes without control characters.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Id(String);

impl Id {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Id {
    type Error = ContractError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
            return Err(ContractError::new(
                "v1: identifier must be 1-256 bytes without control characters",
            ));
        }
        Ok(Self(value))
    }
}

impl From<Id> for String {
    fn from(value: Id) -> Self {
        value.0
    }
}

/// `sha256:` followed by exactly 64 lowercase hex digits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Digest(String);

impl Digest {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Digest {
    type Error = ContractError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.strip_prefix("sha256:") {
            Some(hex)
                if hex.len() == 64
                    && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) =>
            {
                Ok(Self(value))
            }
            _ => Err(ContractError::new(
                "v1: digest must be sha256: followed by 64 lowercase hex digits",
            )),
        }
    }
}

impl From<Digest> for String {
    fn from(value: Digest) -> Self {
        value.0
    }
}

/// An RFC 3339 UTC timestamp, `YYYY-MM-DDTHH:MM:SS[.fraction]Z`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Deadline(String);

impl Deadline {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Deadline {
    type Error = ContractError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if valid_utc_timestamp(&value) {
            Ok(Self(value))
        } else {
            Err(ContractError::new(
                "v1: deadline must be an RFC 3339 UTC timestamp ending in Z",
            ))
        }
    }
}

impl From<Deadline> for String {
    fn from(value: Deadline) -> Self {
        value.0
    }
}

fn valid_utc_timestamp(text: &str) -> bool {
    let Some(text) = text.strip_suffix('Z') else {
        return false;
    };
    let (main, fraction) = match text.split_once('.') {
        Some((main, fraction)) => (main, Some(fraction)),
        None => (text, None),
    };
    if fraction.is_some_and(|f| f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit())) {
        return false;
    }
    let bytes = main.as_bytes();
    if !main.is_ascii()
        || bytes.len() != 19
        || [4, 7, 13, 16]
            .iter()
            .zip(b"--::")
            .any(|(&i, &c)| bytes[i] != c)
        || bytes[10] != b'T'
    {
        return false;
    }
    let number = |range: std::ops::Range<usize>| -> Option<u32> {
        let part = &main[range];
        part.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| part.parse().ok())
            .flatten()
    };
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) = (
        number(0..4),
        number(5..7),
        number(8..10),
        number(11..13),
        number(14..16),
        number(17..19),
    ) else {
        return false;
    };
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&day) && hour < 24 && minute < 60 && second < 60
}
