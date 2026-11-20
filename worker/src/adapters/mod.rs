//! Fail-closed legacy language adapters. Qualification never enables a production profile.
pub mod compat;
pub mod compat_go;
pub mod javascript;

use crate::{
    api::{Code, ExecuteRequest, Reason, Refusal},
    host::Context,
};

const MATRIX: &str = include_str!("language-profile-matrix.json");

pub struct Registry;

/// Fixed adapter diagnostics stay separate from the frozen execution API refusal shape.
#[derive(Debug)]
pub struct AdapterError {
    pub refusal: Refusal,
    pub message: &'static str,
}

impl AdapterError {
    /// Adapter refusals answer a fully parsed request, retaining its activation identity.
    pub fn response(&self, request: &ExecuteRequest) -> (u16, Vec<u8>) {
        request.failure(self.refusal.clone(), Vec::new())
    }
    pub(super) fn blocked(message: &'static str) -> Self {
        Self {
            refusal: Refusal::new(Code::ExecutionFailed, Reason::ArtifactRejected),
            message,
        }
    }
}

impl Registry {
    pub fn invoke(
        &self,
        request: &ExecuteRequest,
        context: Option<&Context>,
        source: &str,
        manifest: &str,
        guest: impl FnOnce(javascript::Input) -> javascript::Outcome,
    ) -> Result<(u16, Vec<u8>), AdapterError> {
        let input = javascript::prepare(request, context, source, manifest)?;
        let matrix: serde_json::Value = serde_json::from_str(MATRIX)
            .map_err(|_| AdapterError::blocked("javascript profile disabled"))?;
        let profile = &matrix["profiles"]["javascript"];
        // The frozen execution API exposes the profile through its serialized contract.
        // Check the pin even while disabled, so both refusal gates remain exercised.
        let wire = serde_json::to_value(request)
            .map_err(|_| AdapterError::blocked("javascript qualification missing"))?;
        let pinned = &wire["projection"]["language_profile"];
        if pinned["name"] != profile["id"] || pinned["version"] != profile["toolchainVersion"] {
            return Err(AdapterError::blocked("javascript qualification missing"));
        }
        if profile["status"] != "enabled" {
            return Err(AdapterError::blocked("javascript profile disabled"));
        }
        Ok(javascript::finish(request, guest(input)))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn matrix_copy_is_byte_identical_to_the_contract() {
        assert_eq!(
            super::MATRIX.as_bytes(),
            include_bytes!("../../../docs/contracts/language-profile-matrix.json")
        );
    }
}
