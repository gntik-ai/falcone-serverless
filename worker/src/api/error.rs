//! Parse errors. They carry enums only, never a value taken from the input.
use std::fmt;

/// Why the JSON could not be read into the contract types. This is serde's error category;
/// its message is dropped because it quotes the offending input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Syntax,
    Data,
    Eof,
}

/// A document that is well formed but breaks a cross-field rule of the contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Violation {
    /// The credential claims do not bind this activation, attempt, revision or lifetime.
    CredentialMismatch,
    /// The credential allows a capability the projection does not grant.
    CapabilityNotGranted,
    /// The invocation identity or the result identity disagrees with the request.
    IdentityMismatch,
    /// The result names another activation or attempt than the request.
    CorrelationMismatch,
    /// A base is pinned and `identity.base_digest` is absent.
    BaseDigestMissing,
    /// No base is pinned and `identity.base_digest` is present.
    BaseDigestUnexpected,
    /// The result status and its `result` / `failure` members disagree.
    StatusShape,
    /// The failure class differs from the class table for its code.
    ClassMismatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    Malformed(Category),
    Invalid(Violation),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(category) => write!(f, "malformed document ({category:?})"),
            Self::Invalid(violation) => write!(f, "contract violation ({violation:?})"),
        }
    }
}

impl std::error::Error for ParseError {}

impl From<serde_json::Error> for ParseError {
    fn from(error: serde_json::Error) -> Self {
        use serde_json::error::Category as Source;
        Self::Malformed(match error.classify() {
            Source::Data => Category::Data,
            Source::Eof => Category::Eof,
            Source::Io | Source::Syntax => Category::Syntax,
        })
    }
}

impl From<Violation> for ParseError {
    fn from(violation: Violation) -> Self {
        Self::Invalid(violation)
    }
}
