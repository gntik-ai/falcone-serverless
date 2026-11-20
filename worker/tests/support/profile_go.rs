//! Shared checks over trusted-built components; no fixture compilation or execution here.
use falcone_serverless_worker::artifact::validate;
use std::path::PathBuf;
use wasmparser::{Parser, Payload, Validator, WasmFeatures};

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
    Validator::new_with_features(WasmFeatures::default() | WasmFeatures::COMPONENT_MODEL)
        .validate_all(bytes)
        .map_err(|_| "invalid component or nested core module".to_owned())?;
    let mut allocators = 0;
    for payload in Parser::new(0).parse_all(bytes) {
        if let Payload::ExportSection(exports) = payload.map_err(|_| "invalid core module")? {
            for export in exports {
                if export.map_err(|_| "invalid core export")?.name == "cabi_realloc" {
                    allocators += 1;
                }
            }
        }
    }
    if allocators != 1 {
        return Err(format!(
            "expected exactly one core cabi_realloc export; found {allocators}"
        ));
    }
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
