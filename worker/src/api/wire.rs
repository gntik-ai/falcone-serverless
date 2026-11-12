//! Serde helpers and redaction macros shared by the API types.
//!
//! Redaction by construction: no type in this module tree derives `Debug`. Every type that can
//! hold a request-controlled value implements `Debug` and `Display` through `redacted!`, which
//! never reads the value. Plain enums print only their variant name through `unit_debug!`.

use serde::{Deserialize, Deserializer, de::Error as _};

/// Reads an optional field whose contract says "absent", never `null`.
///
/// With `#[serde(default)]`, an absent key stays `None`, a value becomes `Some`, and an explicit
/// `null` is an error. Absent, `null` and a value are three distinct cases.
pub(crate) fn present<'de, D, T>(input: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(input)?
        .map(Some)
        .ok_or_else(|| D::Error::custom("explicit null"))
}

/// `Debug` and `Display` that print only the type name and a marker, never the value.
macro_rules! redacted {
    ($($ty:ty),+ $(,)?) => {$(
        impl ::std::fmt::Debug for $ty {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(concat!(stringify!($ty), "(<redacted>)"))
            }
        }
        impl ::std::fmt::Display for $ty {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(concat!(stringify!($ty), "(<redacted>)"))
            }
        }
    )+};
}

/// `Debug` for an enum of unit variants: the variant name is a validated code, not request data.
macro_rules! unit_debug {
    ($ty:ident: $($variant:ident),+ $(,)?) => {
        impl ::std::fmt::Debug for $ty {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(match self { $(Self::$variant => stringify!($variant)),+ })
            }
        }
    };
}

/// A constant string field (`version`, `type`) that serializes to exactly its literal.
macro_rules! constant {
    ($ty:ident, $literal:literal) => {
        #[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
        pub enum $ty {
            #[serde(rename = $literal)]
            Only,
        }
        $crate::api::wire::unit_debug!($ty: Only);
    };
}

pub(crate) use {constant, redacted, unit_debug};
