use sha2::{Digest, Sha256};
use wasmparser::{ComponentTypeRef, Encoding, Parser, Payload, Validator, WasmFeatures};

use super::Error;

pub const ALLOWED_IMPORTS: [&str; 8] = [
    "falcone:host/storage@0.1.0",
    "falcone:host/http@0.1.0",
    "falcone:host/events@0.1.0",
    "falcone:host/workflows@0.1.0",
    "falcone:host/logging@0.1.0",
    "falcone:host/cancellation@0.1.0",
    "falcone:host/progress@0.1.0",
    "falcone:host/invocation-context@0.1.0",
];

#[derive(Debug)]
pub struct Validated<'a> {
    bytes: &'a [u8],
    pub artifact_digest: [u8; 32],
    pub imports: Vec<String>,
}

impl<'a> Validated<'a> {
    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }
}

/// No process may receive bytes until this raw-component boundary succeeds.
/// Profile guest-memory checks belong to RUN-01, not this compile-only boundary.
pub fn validate(bytes: &[u8], input_bytes: usize) -> Result<Validated<'_>, Error> {
    if bytes.len() > input_bytes {
        return Err(Error::new("compile_size_limit_exceeded"));
    }
    if !bytes.starts_with(b"\0asm") {
        return Err(Error::new("precompiled_rejected"));
    }
    let invalid = || Error::new("invalid_module");
    Validator::new_with_features(WasmFeatures::default() | WasmFeatures::COMPONENT_MODEL)
        .validate_all(bytes)
        .map_err(|_| invalid())?;
    let mut depth = 0;
    let mut imports = Vec::new();
    for payload in Parser::new(0).parse_all(bytes) {
        match payload.map_err(|_| invalid())? {
            Payload::Version { encoding, .. } if depth == 0 => {
                if encoding != Encoding::Component {
                    return Err(invalid());
                }
            }
            Payload::ModuleSection { .. } | Payload::ComponentSection { .. } => depth += 1,
            Payload::End(_) if depth > 0 => depth -= 1,
            Payload::ComponentImportSection(section) if depth == 0 => {
                for import in section {
                    let import = import.map_err(|_| invalid())?;
                    let name = import.name.name;
                    if !ALLOWED_IMPORTS.contains(&name)
                        || !matches!(import.ty, ComponentTypeRef::Instance(_))
                    {
                        return Err(Error {
                            code: "undeclared_import",
                            // Bound diagnostic output; never include the raw artifact.
                            import: Some(name.chars().take(256).collect()),
                        });
                    }
                    imports.push(name.to_owned());
                }
            }
            _ => {}
        }
    }
    Ok(Validated {
        bytes,
        artifact_digest: Sha256::digest(bytes).into(),
        imports,
    })
}
