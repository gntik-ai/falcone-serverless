//! Legacy source compatibility and admission. A supported source is not a qualified profile.
pub mod compat;
pub mod javascript;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistryError {
    Blocked,
    Disabled,
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Blocked => "function blocked by compatibility check",
            Self::Disabled => "language profile disabled",
        })
    }
}

/// The immutable matrix is the enablement authority. There is no caller-supplied switch,
/// status override, external registry file or fallback execution mode.
pub fn admit_profile(id: &str, verdict: &compat::Verdict) -> Result<(), RegistryError> {
    if !verdict.is_supported() {
        return Err(RegistryError::Blocked);
    }
    let matrix: serde_json::Value = serde_json::from_str(include_str!(
        "../../../docs/contracts/language-profile-matrix.json"
    ))
    .map_err(|_| RegistryError::Disabled)?;
    let profile = &matrix["profiles"]["javascript"];
    if profile["id"].as_str() == Some(id) && profile["status"].as_str() == Some("enabled") {
        Ok(())
    } else {
        Err(RegistryError::Disabled)
    }
}
