use sha2::{Digest, Sha256};
use wasmparser::component_types::{
    ComponentAnyTypeId, ComponentDefinedType, ComponentEntityType, ComponentFuncType,
    ComponentValType,
};
use wasmparser::types::Types;
use wasmparser::{
    ComponentTypeRef, Encoding, Parser, Payload, PrimitiveValType, Validator, WasmFeatures,
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

/// The one export a component must have: `handle` of `falcone:host/handler`.
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
            Payload::ComponentExportSection(section) if depth == 0 => {
                exports += section.count() as usize;
            }
            _ => {}
        }
    }
    // Static type information only: no component is compiled or instantiated, so no
    // start function can run before the handler's signature is known.
    if exports != 1 || !handler_export_is_exact(&types) {
        return Err(Error::new("missing_handler_export"));
    }
    Ok(Validated {
        bytes,
        artifact_digest: Sha256::digest(bytes).into(),
        imports,
    })
}

/// `falcone:host/handler@0.2.0` is an instance that exports `handle` with exactly the WIT
/// signature. It may also export the `handler-error` type that `handle` returns.
fn handler_export_is_exact(types: &Types) -> bool {
    let Some(item) = types.component_item_for_export(HANDLER_EXPORT) else {
        return false;
    };
    let ComponentEntityType::Instance(instance) = item.ty else {
        return false;
    };
    let mut handle = false;
    for (name, export) in &types[instance].exports {
        match (name.as_str(), export.ty) {
            ("handle", ComponentEntityType::Func(function)) => {
                handle = is_handle(types, &types[function]);
            }
            (
                "handler-error",
                ComponentEntityType::Type {
                    created: ComponentAnyTypeId::Defined(error),
                    ..
                },
            ) => {
                if !is_handler_error(types, ComponentValType::Type(error)) {
                    return false;
                }
            }
            _ => return false,
        }
    }
    handle
}

fn is_handle(types: &Types, function: &ComponentFuncType) -> bool {
    let [(name, params)] = &*function.params else {
        return false;
    };
    let Some(ComponentValType::Type(result)) = function.result else {
        return false;
    };
    let ComponentDefinedType::Result {
        ok: Some(ok),
        err: Some(err),
        ..
    } = &types[result]
    else {
        return false;
    };
    !function.async_
        && name.as_str() == "params"
        && is_bytes(types, *params)
        && is_bytes(types, *ok)
        && is_handler_error(types, *err)
}

fn is_bytes(types: &Types, ty: ComponentValType) -> bool {
    matches!(
        ty,
        ComponentValType::Type(id)
            if matches!(
                &types[id],
                ComponentDefinedType::List {
                    element: ComponentValType::Primitive(PrimitiveValType::U8),
                    ..
                }
            )
    )
}

/// `variant { invalid-input(string), failed(string) }`, with the cases in this order.
fn is_handler_error(types: &Types, ty: ComponentValType) -> bool {
    let ComponentValType::Type(id) = ty else {
        return false;
    };
    let ComponentDefinedType::Variant(variant) = &types[id] else {
        return false;
    };
    let is_string = |ty: Option<ComponentValType>| {
        matches!(
            ty,
            Some(ComponentValType::Primitive(PrimitiveValType::String))
        )
    };
    let mut cases = variant.cases.iter();
    matches!(
        (cases.next(), cases.next(), cases.next()),
        (Some((first, a)), Some((second, b)), None)
            if first.as_str() == "invalid-input"
                && second.as_str() == "failed"
                && is_string(a.ty)
                && is_string(b.ty)
    )
}
