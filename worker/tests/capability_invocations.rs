pub use falcone_serverless_worker::{artifact, host};
#[path = "../src/qualification/capability_support.rs"]
mod capability_support;

#[path = "../src/qualification/counting_alloc.rs"]
mod counting_alloc;

#[path = "../src/qualification/capability_invocations.rs"]
mod qualification;

#[test]
fn narrow_wasi_import_gate() {
    use crate::artifact::validate::validate;
    let allowed = [
        "wasi:random/random@0.2.0",
        "wasi:cli/stdout@0.2.0",
        "wasi:io/streams@0.2.0",
        "wasi:io/error@0.2.0",
    ];
    let denied = [
        "wasi:random/insecure@0.2.0",
        "wasi:random/insecure-seed@0.2.0",
        "wasi:cli/environment@0.2.0",
        "wasi:cli/stdin@0.2.0",
        "wasi:cli/stderr@0.2.0",
        "wasi:cli/exit@0.2.0",
        "wasi:clocks/wall-clock@0.2.0",
        "wasi:io/poll@0.2.0",
    ];
    let base = include_str!("fixtures/was-02/allowed.wat");
    for name in allowed.into_iter().chain(denied) {
        let source = base.replacen(
            "(component",
            &format!("(component (import \"{name}\" (instance))"),
            1,
        );
        let bytes = wat::parse_str(source).unwrap();
        let result = validate(&bytes, bytes.len());
        if allowed.contains(&name) {
            assert!(result.is_ok(), "{name}: {result:?}");
        } else {
            assert_eq!(result.unwrap_err().code, "undeclared_import", "{name}");
        }
    }
    assert_eq!(
        validate(b"\x7fELFprecompiled", 100).unwrap_err().code,
        "precompiled_rejected"
    );
}
