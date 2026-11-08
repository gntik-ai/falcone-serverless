use sha2::{Digest, Sha256};
use wasmparser::{
    ComponentExternalKind, ComponentTypeRef, Encoding, Parser, Payload, Validator, WasmFeatures,
};

use super::Error;

pub const ALLOWED_IMPORTS: [&str; 8] = [
    "falcone:host/storage@0.2.0",
    "falcone:host/http@0.2.0",
    "falcone:host/events@0.2.0",
    "falcone:host/workflows@0.2.0",
    "falcone:host/logging@0.2.0",
    "falcone:host/cancellation@0.2.0",
    "falcone:host/progress@0.2.0",
    "falcone:host/invocation-context@0.2.0",
];

/// The one top-level export a component must have, as an instance (world `function`).
pub const HANDLER_EXPORT: &str = "falcone:host/handler@0.2.0";

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
    let mut handler_exports = 0;
    let mut other_exports = 0;
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
            Payload::ComponentExportSection(section) if depth == 0 => {
                for export in section {
                    let export = export.map_err(|_| invalid())?;
                    let name = export.name;
                    if name.name == HANDLER_EXPORT
                        && name.implements.is_none()
                        && name.version_suffix.is_none()
                        && name.external_id.is_none()
                        && export.kind == ComponentExternalKind::Instance
                    {
                        handler_exports += 1;
                    } else {
                        other_exports += 1;
                    }
                }
            }
            _ => {}
        }
    }
    // Exactly the handler: a missing, different or additional export fails closed. The
    // diagnostic is a code only, so no artifact-supplied export name is ever echoed.
    if handler_exports != 1 || other_exports != 0 {
        return Err(Error::new("missing_handler_export"));
    }
    Ok(Validated {
        bytes,
        artifact_digest: Sha256::digest(bytes).into(),
        imports,
    })
}
