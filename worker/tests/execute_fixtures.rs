//! Binary fixtures embedded in the cluster image have independent WAT oracles.
#[test]
fn embedded_execute_components_match_the_wat_sources() {
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/qualification/execute_fixtures");
    for name in [
        "echo",
        "actions",
        "start-empty",
        "start-signature",
        "utf16",
        "latin1",
        "host-utf16",
        "host-latin1",
    ] {
        assert_eq!(
            wat::parse_file(root.join(format!("{name}.wat"))).unwrap(),
            std::fs::read(root.join(format!("{name}.wasm"))).unwrap(),
            "{name}"
        );
    }
}
