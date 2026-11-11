//! Shared wire types: validated strings, digests, the revision projection and the base union.

use super::error::Malformed;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Number;
use std::fmt;

/// A value that never appears in formatted output (payloads, log lines).
#[derive(Clone, PartialEq, Deserialize, Serialize)]
#[serde(transparent)]
pub struct Redacted<T>(T);

impl<T> Redacted<T> {
    pub fn new(value: T) -> Self {
        Self(value)
    }

    pub fn get(&self) -> &T {
        &self.0
    }
}

impl<T> fmt::Debug for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// A non-empty request string of at most 256 characters. Its `Debug` output is redacted
/// because any such string may be attacker-chosen or credential-like.
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct Name(String);

impl Name {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// For compile-time constants only; request text goes through deserialization.
    pub(super) fn trusted(text: &'static str) -> Self {
        Self(text.to_owned())
    }
}

impl TryFrom<String> for Name {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.chars().count() {
            1..=256 => Ok(Self(value)),
            _ => Err("name length"),
        }
    }
}

impl From<Name> for String {
    fn from(name: Name) -> String {
        name.0
    }
}

impl fmt::Debug for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// A non-empty string of any length (credential claims and grants have no upper bound in
/// their published schemas). Its `Debug` output is redacted like [`Name`].
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct Text(String);

impl Text {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Text {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() {
            Err("empty text")
        } else {
            Ok(Self(value))
        }
    }
}

impl From<Text> for String {
    fn from(text: Text) -> String {
        text.0
    }
}

impl fmt::Debug for Text {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// The largest integer every JSON implementation reads exactly (2^53 - 1): the `maximum` the
/// published schemas give every unbounded integer member.
pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// A JSON `integer` the way JSON Schema reads it: `1`, `1.0` and `1e0` are the same integer.
fn whole(number: &Number) -> Option<u64> {
    if let Some(value) = number.as_u64() {
        return Some(value);
    }
    let value = number.as_f64()?;
    (value.fract() == 0.0 && (0.0..=MAX_SAFE_INTEGER as f64).contains(&value))
        .then_some(value as u64)
}

macro_rules! bounded_integer {
    ($(#[$doc:meta])* $name:ident($inner:ty), $min:expr, $max:expr, $error:literal) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
        #[serde(try_from = "Number", into = "u64")]
        pub struct $name($inner);

        impl $name {
            pub fn get(self) -> $inner {
                self.0
            }
        }

        impl TryFrom<Number> for $name {
            type Error = &'static str;

            fn try_from(number: Number) -> Result<Self, Self::Error> {
                whole(&number)
                    .filter(|value| ($min..=$max).contains(value))
                    .and_then(|value| <$inner>::try_from(value).ok())
                    .map(Self)
                    .ok_or($error)
            }
        }

        impl From<$name> for u64 {
            fn from(value: $name) -> u64 {
                u64::from(value.0)
            }
        }
    };
}

bounded_integer!(
    /// A positive count (budgets, update sequence): `1..=2^53-1`.
    Count(u64), 1, MAX_SAFE_INTEGER, "count"
);
bounded_integer!(
    /// The attempt number: `1..=4294967295`.
    AttemptNumber(u32), 1, u32::MAX as u64, "attempt_number"
);
bounded_integer!(
    /// A progress percentage: `0..=100`.
    Percent(u8), 0, 100, "percent"
);
bounded_integer!(
    /// An HTTP status code: `100..=599`.
    StatusCode(u16), 100, 599, "status_code"
);

/// `sha256:` followed by 64 lowercase hex digits.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct Digest(String);

impl Digest {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Digest {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.strip_prefix("sha256:") {
            Some(hex)
                if hex.len() == 64
                    && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) =>
            {
                Ok(Self(value))
            }
            _ => Err("digest"),
        }
    }
}

impl From<Digest> for String {
    fn from(digest: Digest) -> String {
        digest.0
    }
}

/// The constant `contract_version` member.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct V1;

impl TryFrom<String> for V1 {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value == super::CONTRACT_VERSION {
            Ok(Self)
        } else {
            Err("contract_version")
        }
    }
}

impl From<V1> for String {
    fn from(_: V1) -> String {
        super::CONTRACT_VERSION.to_owned()
    }
}

/// A UTC RFC 3339 timestamp: `YYYY-MM-DDTHH:MM:SS[.fraction]Z`, a real calendar date.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct Deadline(String);

impl Deadline {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Deadline {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if valid_timestamp(&value) {
            Ok(Self(value))
        } else {
            Err("deadline")
        }
    }
}

impl From<Deadline> for String {
    fn from(deadline: Deadline) -> String {
        deadline.0
    }
}

fn valid_timestamp(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() < 20 || bytes[bytes.len() - 1] != b'Z' {
        return false;
    }
    let number = |range: std::ops::Range<usize>| -> Option<u32> {
        let digits = bytes.get(range)?;
        if !digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
        digits
            .iter()
            .try_fold(0u32, |sum, d| Some(sum * 10 + u32::from(d - b'0')))
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
    if [bytes[4], bytes[7], bytes[10], bytes[13], bytes[16]] != *b"--T::" {
        return false;
    }
    let fraction = &bytes[19..bytes.len() - 1];
    let fraction_ok = fraction.is_empty()
        || (fraction[0] == b'.'
            && (2..=10).contains(&fraction.len())
            && fraction[1..].iter().all(u8::is_ascii_digit));
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    fraction_ok && (1..=days).contains(&day) && hour < 24 && minute < 60 && second < 60
}

/// Absent, explicit `null` and a value are three different states on the wire. A field the
/// contract says is absent must be absent: validation rejects [`Presence::Null`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Presence<T> {
    Absent,
    Null,
    Value(T),
}

impl<T> Presence<T> {
    /// The serde default for a member that may be missing (`T` need not be `Default`).
    pub fn absent() -> Self {
        Self::Absent
    }

    pub fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }

    /// The value when present, `None` when absent; an explicit `null` is malformed.
    pub fn value(&self) -> Result<Option<&T>, Malformed> {
        match self {
            Self::Absent => Ok(None),
            Self::Null => Err(Malformed::Rule("null_not_absent")),
            Self::Value(value) => Ok(Some(value)),
        }
    }
}

impl<T> From<Option<T>> for Presence<T> {
    fn from(value: Option<T>) -> Self {
        value.map_or(Self::Absent, Self::Value)
    }
}

/// `deserialize_with` for a field that may be absent: pair it with `#[serde(default)]`, so
/// that a missing key is [`Presence::Absent`] and a present `null` is [`Presence::Null`].
pub fn present<'de, D, T>(deserializer: D) -> Result<Presence<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(match Option::<T>::deserialize(deserializer)? {
        Some(value) => Presence::Value(value),
        None => Presence::Null,
    })
}

impl<T: Serialize> Serialize for Presence<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Value(value) => value.serialize(serializer),
            Self::Absent | Self::Null => serializer.serialize_none(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeMode {
    Wasm,
    Oci,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageProfile {
    pub name: Name,
    pub version: Name,
}

/// A catalog base pinned by name, concrete version and digest.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogBase {
    pub id: Name,
    pub version: Name,
    pub digest: Digest,
}

/// A bring-your-own image pinned by digest, with its signer identity and attestation.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ByoBase {
    pub image_digest: Digest,
    pub signer_identity: Name,
    pub attestation_ref: Name,
}

/// The tagged base union. A revision that pins no base omits the member entirely; an
/// unresolved default alias is not a variant and fails to parse.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Base {
    Catalog(CatalogBase),
    Byo(ByoBase),
}

impl Base {
    /// The digest a result's `base_digest` must repeat.
    pub fn digest(&self) -> &Digest {
        match self {
            Self::Catalog(base) => &base.digest,
            Self::Byo(base) => &base.image_digest,
        }
    }

    /// A catalog base names a concrete `id@version`; aliases such as `default` or `latest`
    /// are resolved by Falcone before the revision is pinned.
    pub(super) fn check(&self) -> Result<(), Malformed> {
        if let Self::Catalog(base) = self {
            let id = base.id.as_str();
            let version = base.version.as_str();
            let id_ok = id.len() <= 128
                && id.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
                && id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c));
            let version_ok = version.len() <= 64
                && version.starts_with(|c: char| c.is_ascii_digit())
                && version
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || ".+_-".contains(c));
            if !(id_ok && version_ok) {
                return Err(Malformed::Rule("base_not_concrete"));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Budgets {
    pub fuel: Count,
    pub memory_bytes: Count,
    pub timeout_ms: Count,
    pub max_host_calls: Count,
}

/// The pinned revision as the worker sees it. Unknown members are ignored so that later
/// revisions of the contract stay additive; only the members below carry meaning in v1.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Projection {
    pub revision: Name,
    pub runtime_mode: RuntimeMode,
    #[serde(
        default = "Presence::absent",
        deserialize_with = "present",
        skip_serializing_if = "Presence::is_absent"
    )]
    pub base: Presence<Base>,
    pub artifact_digest: Digest,
    pub language_profile: LanguageProfile,
    #[serde(
        default = "Presence::absent",
        deserialize_with = "present",
        skip_serializing_if = "Presence::is_absent"
    )]
    pub component_digest: Presence<Digest>,
    pub engine_version: Name,
    pub wasi_version: Name,
    pub interface_version: Name,
    pub declared_imports: Vec<Name>,
    pub budgets: Budgets,
    pub granted_capabilities: Vec<super::grant::Grant>,
}

impl Projection {
    /// The pinned base, if any; an explicit `null` base is malformed.
    pub fn pinned_base(&self) -> Result<Option<&Base>, Malformed> {
        self.base.value()
    }

    pub(super) fn check(&self) -> Result<(), Malformed> {
        if let Some(base) = self.pinned_base()? {
            base.check()?;
        }
        self.component_digest.value()?;
        self.granted_capabilities
            .iter()
            .try_for_each(|grant| grant.check(super::REQUEST_GRANT_VERSION))
    }
}
