use sha2::{Digest, Sha256};
use wasmparser::component_types::{
    ComponentDefinedType, ComponentEntityType, ComponentFuncType, ComponentValType,
};
use wasmparser::types::TypesRef;
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

/// The one top-level export a function component must have: an instance of the WIT
/// `handler` interface, whose `handle` is checked against the WIT signature below.
pub const HANDLER_EXPORT: &str = "falcone:host/handler@0.2.0";

/// Diagnostics carry at most this many characters of any guest-supplied name.
const DIAGNOSTIC_CHARS: usize = 256;

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
///
/// For a function component: the import policy plus the handler export gate.
pub fn validate(bytes: &[u8], input_bytes: usize) -> Result<Validated<'_>, Error> {
    validate_component(bytes, input_bytes, true)
}

/// For a catalog base component, which is composed into function components later and so
/// has no handler of its own: the same import policy, without the handler export gate.
/// Never used to admit a component for execution.
pub fn validate_catalog(bytes: &[u8], input_bytes: usize) -> Result<Validated<'_>, Error> {
    validate_component(bytes, input_bytes, false)
}

fn validate_component(
    bytes: &[u8],
    input_bytes: usize,
    handler_export: bool,
) -> Result<Validated<'_>, Error> {
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
    let mut exports = Vec::new();
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
                            export: None,
                        });
                    }
                    imports.push(name.to_owned());
                }
            }
            Payload::ComponentExportSection(section) if depth == 0 => {
                for export in section {
                    let export = export.map_err(|_| invalid())?;
                    exports.push(export.name.name.to_owned());
                }
            }
            _ => {}
        }
    }
    // Static, before any compilation or instantiation: no guest code (start functions
    // included) can have reached a host capability when a component is rejected here.
    if handler_export {
        check_handler(types.as_ref(), &exports)?;
    }
    Ok(Validated {
        bytes,
        artifact_digest: Sha256::digest(bytes).into(),
        imports,
    })
}

fn export_error(code: &'static str, name: Option<&str>) -> Error {
    Error {
        code,
        import: None,
        export: name.map(|name| name.chars().take(DIAGNOSTIC_CHARS).collect()),
    }
}

/// Require exactly one top-level export, the handler instance, whose `handle` has the
/// exact WIT signature: `func(params: list<u8>) -> result<list<u8>, handler-error>`
/// with `variant handler-error { invalid-input(string), failed(string) }`.
fn check_handler(types: TypesRef<'_>, exports: &[String]) -> Result<(), Error> {
    if let Some(other) = exports.iter().find(|name| *name != HANDLER_EXPORT) {
        return Err(export_error("missing_handler_export", Some(other)));
    }
    if exports.is_empty() {
        return Err(export_error("missing_handler_export", None));
    }
    let Some(ComponentEntityType::Instance(instance)) = types
        .component_item_for_export(HANDLER_EXPORT)
        .map(|item| item.ty)
    else {
        return Err(export_error("missing_handler_export", Some(HANDLER_EXPORT)));
    };
    let invalid = || export_error("invalid_handler_signature", Some(HANDLER_EXPORT));
    let mut handle = None;
    for (name, item) in &types[instance].exports {
        match (name.as_str(), item.ty) {
            ("handle", ComponentEntityType::Func(func)) => handle = Some(&types[func]),
            // The WIT interface also exports its `handler-error` type; nothing else.
            ("handler-error", ComponentEntityType::Type { .. }) => {}
            _ => return Err(invalid()),
        }
    }
    if handle.is_some_and(|func| handle_signature(types, func)) {
        Ok(())
    } else {
        Err(invalid())
    }
}

fn defined<'a>(types: TypesRef<'a>, ty: ComponentValType) -> Option<&'a ComponentDefinedType> {
    match ty {
        ComponentValType::Type(id) => types.get(id),
        ComponentValType::Primitive(_) => None,
    }
}

fn is(ty: ComponentValType, primitive: PrimitiveValType) -> bool {
    matches!(ty, ComponentValType::Primitive(actual) if actual == primitive)
}

fn handle_signature(types: TypesRef<'_>, func: &ComponentFuncType) -> bool {
    let list_u8 = |ty| {
        matches!(
            defined(types, ty),
            Some(ComponentDefinedType::List { element, .. }) if is(*element, PrimitiveValType::U8)
        )
    };
    let [(name, param)] = &*func.params else {
        return false;
    };
    if func.async_ || name.as_str() != "params" || !list_u8(*param) {
        return false;
    }
    let Some(ComponentDefinedType::Result {
        ok: Some(ok),
        err: Some(err),
        ..
    }) = func.result.and_then(|result| defined(types, result))
    else {
        return false;
    };
    let Some(ComponentDefinedType::Variant(variant)) = defined(types, *err) else {
        return false;
    };
    // Case order fixes the canonical-ABI discriminants, so it is part of the signature.
    list_u8(*ok)
        && variant.cases.len() == 2
        && variant
            .cases
            .iter()
            .zip(["invalid-input", "failed"])
            .all(|((name, case), expected)| {
                name.as_str() == expected
                    && case.ty.is_some_and(|ty| is(ty, PrimitiveValType::String))
            })
}
