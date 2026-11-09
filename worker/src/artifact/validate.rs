use sha2::{Digest, Sha256};
use wasmparser::{
    CanonicalFunction, CanonicalOption, ComponentTypeRef, Encoding, Parser, Payload, Validator,
    WasmFeatures,
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
/// Export types and canonical options are checked without compiling or running guest code.
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
    let mut handler = false;
    for payload in Parser::new(0).parse_all(bytes) {
        match payload.map_err(|_| invalid())? {
            Payload::Version { encoding, .. } if depth == 0 => {
                if encoding != Encoding::Component {
                    return Err(invalid());
                }
            }
            Payload::ModuleSection { .. } | Payload::ComponentSection { .. } => depth += 1,
            Payload::End(_) if depth > 0 => depth -= 1,
            // Check every nested component as well: an adapter's canonical
            // options can otherwise transcode a string before the host sees it.
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
                    if export.name.name != HANDLER_EXPORT
                        || export.kind != wasmparser::ComponentExternalKind::Instance
                    {
                        return Err(Error::new("missing_handler_export"));
                    }
                    handler = true;
                }
            }
            _ => {}
        }
    }
    if !handler {
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

fn handler_signature(types: &wasmparser::types::Types) -> bool {
    use wasmparser::PrimitiveValType as Primitive;
    use wasmparser::component_types::{
        ComponentDefinedType as Defined, ComponentEntityType as Entity, ComponentValType as Value,
    };
    let Some(item) = types.component_item_for_export(HANDLER_EXPORT) else {
        return false;
    };
    let Entity::Instance(instance) = item.ty else {
        return false;
    };
    let Some(item) = types[instance].exports.get("handle") else {
        return false;
    };
    let Entity::Func(function) = item.ty else {
        return false;
    };
    let function = &types[function];
    if function.async_ || function.params.len() != 1 || function.params[0].0.as_str() != "params" {
        return false;
    }
    let bytes = |value| match value {
        Value::Type(id) => matches!(
            types[id],
            Defined::List {
                element: Value::Primitive(Primitive::U8),
                ..
            }
        ),
        _ => false,
    };
    if !bytes(function.params[0].1) {
        return false;
    }
    let Some(Value::Type(result)) = function.result else {
        return false;
    };
    let Defined::Result {
        ok: Some(ok),
        err: Some(Value::Type(error)),
        ..
    } = types[result]
    else {
        return false;
    };
    if !bytes(ok) {
        return false;
    }
    let Defined::Variant(variant) = &types[error] else {
        return false;
    };
    variant.cases.len() == 2
        && variant
            .cases
            .iter()
            .zip(["invalid-input", "failed"])
            .all(|((name, case), expected)| {
                name.as_str() == expected
                    && matches!(case.ty, Some(Value::Primitive(Primitive::String)))
            })
}
