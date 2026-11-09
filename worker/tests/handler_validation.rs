use falcone_serverless_worker::artifact::validate::validate;

#[test]
fn accepts_handler_and_rejects_missing_and_old_interfaces() {
    let valid = include_bytes!("../src/qualification/execute_fixtures/echo.wasm");
    validate(valid, valid.len()).unwrap();
    for (source, code) in [
        ("(component)", "missing_handler_export"),
        (
            "(component (instance $h) (export \"falcone:host/other@0.2.0\" (instance $h)))",
            "missing_handler_export",
        ),
        (
            "(component (import \"falcone:host/logging@0.1.0\" (instance)))",
            "undeclared_import",
        ),
        (
            "(component (import \"wasi:cli/run@0.2.0\" (instance)))",
            "undeclared_import",
        ),
    ] {
        let bytes = wat::parse_str(source).unwrap();
        assert_eq!(validate(&bytes, bytes.len()).unwrap_err().code, code);
    }
}

#[test]
fn handler_requires_the_exact_signature_before_instantiation() {
    let empty = wat::parse_str(
        "(component (instance $h) (export \"falcone:host/handler@0.2.0\" (instance $h)))",
    )
    .unwrap();
    assert_eq!(
        validate(&empty, empty.len()).unwrap_err().code,
        "invalid_handler_signature"
    );
    let source = include_str!("../src/qualification/execute_fixtures/echo.wat");
    for (before, after) in [
        ("(param \"params\" $bytes)", "(param \"params\" string)"),
        ("(case \"failed\" string)", "(case \"other\" string)"),
        ("(case \"failed\" string)", "(case \"failed\" $bytes)"),
        (
            "(export \"handle\" (func $handle))",
            "(export \"other\" (func $handle))",
        ),
    ] {
        let bytes = wat::parse_str(source.replace(before, after)).unwrap();
        assert_eq!(
            validate(&bytes, bytes.len()).unwrap_err().code,
            "invalid_handler_signature"
        );
    }
}

#[test]
fn rejects_non_utf8_canonical_options_including_nested_components() {
    let source = include_str!("../src/qualification/execute_fixtures/echo.wat");
    for encoding in ["utf16", "latin1+utf16"] {
        let source = source.replace(
            "string-encoding=utf8",
            &format!("string-encoding={encoding}"),
        );
        for source in [source.clone(), format!("(component {source})")] {
            let bytes = wat::parse_str(source).unwrap();
            assert_eq!(
                validate(&bytes, bytes.len()).unwrap_err().code,
                "unsupported_string_encoding"
            );
        }
    }
}
