//! Execution credential, bound to the claims of `ctr-execution-credential` v1.

use super::projection::Grant;
use super::types::{ContractError, Id};
use serde::{Deserialize, Serialize};
use serde_json::Number;
use std::fmt;

const REDACTED: &str = "<REDACTED>";

/// The signed credential string. Its Debug and Display output never contains it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Token(String);

impl Token {
    /// The raw token, for the verification layer only; never log the result.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Token {
    type Error = ContractError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() {
            return Err(ContractError::new("v1: credential token must not be empty"));
        }
        Ok(Self(value))
    }
}

impl From<Token> for String {
    fn from(value: Token) -> Self {
        value.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

/// A non-negative JSON number (`iat`, `exp`), kept exactly as received.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "Number", into = "Number")]
pub struct Timestamp(Number);

impl TryFrom<Number> for Timestamp {
    type Error = ContractError;

    fn try_from(value: Number) -> Result<Self, Self::Error> {
        if value.as_f64().is_some_and(|n| n.is_finite() && n >= 0.0) {
            Ok(Self(value))
        } else {
            Err(ContractError::new("v1: timestamp must be a number >= 0"))
        }
    }
}

impl From<Timestamp> for Number {
    fn from(value: Timestamp) -> Self {
        value.0
    }
}

/// Exactly the properties of `execution-credential.v1.claims.schema.json`; every one
/// is required and none may be added.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionClaims {
    pub iss: Id,
    pub aud: Id,
    pub tenant: Id,
    pub workspace: Id,
    pub pool_profile: Id,
    pub revision: Id,
    pub activation: Id,
    pub attempt: Id,
    pub iat: Timestamp,
    pub exp: Timestamp,
    pub allowed_capabilities: Vec<Grant>,
}

/// The token and the claims it must carry. The worker trusts the claims only after
/// verifying the token as `ctr-execution-credential` v1 defines; this contract checks
/// only that the claims bind to the request.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionCredential {
    token: Token,
    pub claims: ExecutionClaims,
}

impl ExecutionCredential {
    pub fn token(&self) -> &Token {
        &self.token
    }
}

impl fmt::Debug for ExecutionCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExecutionCredential")
            .field("token", &REDACTED)
            .field("claims", &self.claims)
            .finish()
    }
}

impl fmt::Display for ExecutionCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ExecutionCredential({REDACTED})")
    }
}
