use sha2::{Digest, Sha256};
use wasmparser::component_types::{ComponentDefinedType, ComponentEntityType, ComponentValType};
use wasmparser::types::Types;
use wasmparser::{
    CanonicalFunction, CanonicalOption, ComponentTypeRef, Encoding, Parser, Payload,
    PrimitiveValType, Validator, WasmFeatures,
};
pub const HANDLER_EXPORT: &str = "falcone:host/handler@0.2.0";

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
    let types =
        Validator::new_with_features(WasmFeatures::default() | WasmFeatures::COMPONENT_MODEL)
            .validate_all(bytes)
            .map_err(|_| invalid())?;
    let mut depth = 0;
    let mut imports = Vec::new();
    let mut exports = 0;
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
            // Inspect nested components too: their start code can lower a host string.
            Payload::ComponentCanonicalSection(section) => {
                for function in section {
                    let options = match function.map_err(|_| invalid())? {
                        CanonicalFunction::Lift { options, .. }
                        | CanonicalFunction::Lower { options, .. }
                        | CanonicalFunction::TaskReturn { options, .. }
                        | CanonicalFunction::StreamRead { options, .. }
                        | CanonicalFunction::StreamWrite { options, .. }
                        | CanonicalFunction::FutureRead { options, .. }
                        | CanonicalFunction::FutureWrite { options, .. }
                        | CanonicalFunction::ErrorContextNew { options }
                        | CanonicalFunction::ErrorContextDebugMessage { options } => options,
                        _ => continue,
                    };
                    if options.iter().any(|option| {
                        matches!(
                            option,
                            CanonicalOption::UTF16 | CanonicalOption::CompactUTF16
                        )
                    }) {
                        return Err(Error::new("unsupported_string_encoding"));
                    }
                }
            }
            Payload::ComponentExportSection(section) if depth == 0 => {
                for export in section {
                    let export = export.map_err(|_| invalid())?;
                    exports += 1;
                    if export.name.name != HANDLER_EXPORT {
                        return Err(Error::new("missing_handler_export"));
                    }
                }
            }
            _ => {}
        }
    }
    if exports != 1 {
        return Err(Error::new("missing_handler_export"));
    }
    if !handler_signature(&types) {
        return Err(Error::new("invalid_handler_signature"));
    }
    Ok(Validated {
        bytes,
        artifact_digest: Sha256::digest(bytes).into(),
        imports,
    })
}

fn defined(types: &Types, ty: ComponentValType) -> Option<&ComponentDefinedType> {
    match ty {
        ComponentValType::Type(id) => Some(&types[id]),
        _ => None,
    }
}
fn primitive(types: &Types, ty: ComponentValType, expected: PrimitiveValType) -> bool {
    match ty {
        ComponentValType::Primitive(actual) => actual == expected,
        _ => {
            matches!(defined(types, ty), Some(ComponentDefinedType::Primitive(actual)) if *actual == expected)
        }
    }
}
fn bytes_type(types: &Types, ty: ComponentValType) -> bool {
    matches!(defined(types, ty), Some(ComponentDefinedType::List { element, .. }) if primitive(types, *element, PrimitiveValType::U8))
}
fn handler_signature(types: &Types) -> bool {
    let Some(item) = types.component_item_for_export(HANDLER_EXPORT) else {
        return false;
    };
    let ComponentEntityType::Instance(id) = item.ty else {
        return false;
    };
    let Some(handle) = types[id].exports.get("handle") else {
        return false;
    };
    let ComponentEntityType::Func(id) = handle.ty else {
        return false;
    };
    let func = &types[id];
    if func.async_
        || func.params.len() != 1
        || func.params[0].0.as_str() != "params"
        || !bytes_type(types, func.params[0].1)
    {
        return false;
    }
    let Some(ComponentDefinedType::Result {
        ok: Some(ok),
        err: Some(err),
        ..
    }) = func.result.and_then(|ty| defined(types, ty))
    else {
        return false;
    };
    if !bytes_type(types, *ok) {
        return false;
    }
    let Some(ComponentDefinedType::Variant(variant)) = defined(types, *err) else {
        return false;
    };
    variant.cases.len() == 2
        && variant
            .cases
            .iter()
            .zip(["invalid-input", "failed"])
            .all(|((name, case), expected)| {
                name.as_str() == expected
                    && case
                        .ty
                        .is_some_and(|ty| primitive(types, ty, PrimitiveValType::String))
            })
}
