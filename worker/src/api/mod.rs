//! ctr-worker-execution-api v1 request, result and update types.
//!
//! The contract is docs/contracts/worker-execution-api/README.md. These types define
//! the wire shapes and validate them; they execute no guest code.

mod credential;
mod legacy;
mod projection;
mod request;
mod result;
mod types;

pub use credential::{ExecutionClaims, ExecutionCredential, Token};
pub use legacy::{
    LegacyHeaders, LegacyIdentity, LegacyInvocation, LegacyParams, LegacyRejection, LegacyResult,
    MAX_LEGACY_BODY_BYTES,
};
pub use projection::{
    Base, Budgets, ByoBase, CatalogBase, CatalogBaseId, Grant, LanguageProfile, RevisionProjection,
    RuntimeMode,
};
pub use request::{
    CancelMessage, CancelState, ExecuteAttempt, ExecutionUpdate, InvocationIdentity, Percent,
    UpdateBody,
};
pub use result::{
    AdmitError, ExecutionResult, Failure, FailureClass, FailureCode, Refusal, RuntimeIdentity,
    Status, WorkerFacts, admit_wasm_attempt,
};
pub use types::{ContractError, Deadline, Digest, Id};
