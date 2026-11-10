//! Request-controlled values never reach formatted error text, logs or messages.

use serde::{Deserialize, Serialize};
use std::fmt;

/// What `Debug` and `Display` print in place of a request-controlled value.
pub const MARKER: &str = "<redacted>";

/// A request-controlled value. It serializes and deserializes as the plain value, so the
/// wire form is unchanged, but `Debug` and `Display` print only [`MARKER`]. Every free-form
/// string, digest and JSON value taken from a request or result is wrapped in it, so a
/// derived `Debug` on any API type cannot echo input.
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(transparent)]
pub struct Redacted<T>(T);

impl<T> Redacted<T> {
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// The raw value, for comparison and serialization only; never format it.
    pub fn expose(&self) -> &T {
        &self.0
    }
}

impl<T> fmt::Debug for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(MARKER)
    }
}

impl<T> fmt::Display for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(MARKER)
    }
}

/// A `sha256:<64 lowercase hex>` digest; any other form fails to deserialize.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Digest(String);

impl Digest {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(MARKER)
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        let valid = text.strip_prefix("sha256:").is_some_and(|hex| {
            hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        });
        if valid {
            Ok(Self(text))
        } else {
            Err(serde::de::Error::custom("invalid digest"))
        }
    }
}

/// Deserializes a field that must be absent or hold a real value; `null` is not "absent".
pub fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}
