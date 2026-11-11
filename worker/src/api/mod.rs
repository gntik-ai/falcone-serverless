//! ctr-worker-execution-api v1: the execute, update, cancel and result shapes the dispatcher
//! and the worker exchange. The specification is `docs/contracts/worker-execution-api/v1.md`.
//!
//! This module is JSON types and pure validation only. It never executes a guest and never
//! loads a Wasmtime artifact, so it may use serde (`Deserialize`, `deserialize_with`); the
//! `was01_static` guard still rejects every engine artifact-loading API in this directory.
//!
//! Rules every item here follows:
//!
//! * Every validation compiles in every build. Nothing outside `#[cfg(test)] mod tests` is
//!   gated by `cfg(test)`.
//! * No request-controlled string reaches formatted output. Request strings are held in
//!   redacting types, errors carry only enums, and parse failures drop serde's message, which
//!   would echo input. Serialization keeps the contract fields.
//! * Absent and `null` are different. A field whose contract says "absent" rejects `null`.
//! * Evolution is additive: a later minor revision adds optional fields only.
pub mod error;
pub mod failure;
pub mod request;
pub mod result;
pub mod text;
pub mod update;

#[cfg(test)]
mod tests;
