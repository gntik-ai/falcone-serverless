//! Unwired artifact intake primitives; no native cache is loaded or persisted.
pub mod transfer;

#[derive(Debug, PartialEq, Eq)]
pub struct Error {
    pub code: &'static str,
    pub import: Option<String>,
}

impl Error {
    fn new(code: &'static str) -> Self {
        Self { code, import: None }
    }
}
pub mod cache_key;
#[cfg(target_os = "linux")]
pub mod supervisor;
pub mod validate;
