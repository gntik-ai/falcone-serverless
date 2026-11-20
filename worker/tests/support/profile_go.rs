//! Shared checks over trusted-built components; no fixture compilation or execution here.
use falcone_serverless_worker::artifact::validate;
use std::path::PathBuf;
use wasmparser::{Encoding, ExternalKind, Parser, Payload, Validator, WasmFeatures};

pub const MANIFEST: &str = include_str!("../../qualification/profiles/go/manifest.toml");

pub fn components() -> Option<PathBuf> {
    let Some(cache) = std::env::var_os("FS_TOOLCHAIN_CACHE") else {
        println!("UNVERIFIED: FS_TOOLCHAIN_CACHE unset; no Go component built or executed");
        return None;
    };
    Some(
        std::env::var_os("FS_PROFILE_COMPONENTS")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(cache).join("go-profile")),
    )
}

pub fn validate_component(bytes: &[u8]) -> Result<Vec<String>, String> {
    // This is also the precompiled-artifact refusal boundary: no deserializer is called.
    let validated =
        validate::validate(bytes, 64 * 1024 * 1024).map_err(|error| error.code.to_owned())?;
    let manifest: toml::Value = toml::from_str(MANIFEST).unwrap();
    let allowed = manifest["wit"]["interfaces"].as_array().unwrap();
    if validated
        .imports
        .iter()
        .any(|name| !allowed.iter().any(|a| a.as_str() == Some(name)))
    {
        return Err("component import outside Go manifest".into());
    }
    Ok(validated.imports)
}

/// Inspect the TinyGo output before composition. Adapter allocators cannot satisfy
/// this check: the allocator must be a function exported by the handler's core module.
pub fn validate_fixture(bytes: &[u8]) -> Result<(), String> {
    Validator::new_with_features(WasmFeatures::default() | WasmFeatures::COMPONENT_MODEL)
        .validate_all(bytes)
        .map_err(|_| "invalid TinyGo component or core module".to_owned())?;
    let mut stack = Vec::new();
    let mut handlers = Vec::new();
    for payload in Parser::new(0).parse_all(bytes) {
        match payload.map_err(|_| "invalid TinyGo payload")? {
            Payload::Version { encoding, .. } => {
                stack.push((encoding == Encoding::Module).then_some((false, 0)));
            }
            Payload::ExportSection(exports) => {
                if let Some(Some((handler, allocators))) = stack.last_mut() {
                    for export in exports {
                        let export = export.map_err(|_| "invalid core export")?;
                        if export.kind == ExternalKind::Func {
                            *handler |= export.name == "falcone:host/handler@0.2.0#handle";
                            *allocators += usize::from(export.name == "cabi_realloc");
                        }
                    }
                }
            }
            Payload::End(_) => {
                if let Some(Some((true, allocators))) = stack.pop() {
                    handlers.push(allocators);
                }
            }
            _ => {}
        }
    }
    if handlers != [1] {
        return Err(format!(
            "expected one TinyGo handler core module with exactly one cabi_realloc; found {handlers:?}"
        ));
    }
    Ok(())
}
