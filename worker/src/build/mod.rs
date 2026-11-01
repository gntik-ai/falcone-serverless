//! Unwired catalog intake boundary; accepts only validated raw components.

use std::fmt::Write;

#[derive(Debug, PartialEq, Eq)]
pub struct Accepted {
    pub digest: String,
    pub imports: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Rejected {
    pub reason: &'static str,
    pub detail: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Accepted(Accepted),
    Rejected(Rejected),
}

/// Apply the tenant import policy unchanged before handing off the exact bytes.
pub fn validate_catalog_component(
    bytes: &[u8],
    input_bytes: usize,
    mut handoff: impl FnMut(&[u8]),
) -> Verdict {
    if bytes.starts_with(b"\x7fELF") {
        return Verdict::Rejected(Rejected {
            reason: "native_binary",
            detail: "Native binaries are not raw Wasm components".into(),
        });
    }
    match crate::artifact::validate::validate(bytes, input_bytes) {
        Ok(validated) => {
            let mut digest = String::from("sha256:");
            for byte in validated.artifact_digest {
                write!(digest, "{byte:02x}").expect("writing to a String cannot fail");
            }
            let accepted = Verdict::Accepted(Accepted {
                digest,
                imports: validated.imports,
            });
            handoff(bytes);
            accepted
        }
        Err(error) => {
            let (reason, detail) = match error.code {
                "invalid_module" if bytes.starts_with(b"\0asm\x01\0\0\0") => {
                    ("not_a_component", "Expected a Wasm component".into())
                }
                "undeclared_import" => (
                    "forbidden_import",
                    error.import.unwrap_or_else(|| "Forbidden import".into()),
                ),
                _ => ("malformed", "Invalid raw Wasm component".into()),
            };
            Verdict::Rejected(Rejected { reason, detail })
        }
    }
}
