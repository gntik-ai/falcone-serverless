//! The execution API's guest fixtures: each committed binary is the output of its WAT oracle.
//! `REGEN_API_FIXTURES=1 cargo test -p falcone-serverless-worker --test api_fixtures` rewrites them.

use std::path::Path;

#[test]
fn api_fixture_binaries_match_their_wat_oracles() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/qualification/api_fixtures");
    let regenerate = std::env::var_os("REGEN_API_FIXTURES").is_some();
    let mut checked = 0;
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|extension| extension != "wat") {
            continue;
        }
        let expected = wat::parse_file(&path).unwrap();
        let binary = path.with_extension("wasm");
        if regenerate {
            std::fs::write(&binary, &expected).unwrap();
        }
        assert!(
            expected == std::fs::read(&binary).unwrap(),
            "fixture differs: {}",
            path.display()
        );
        checked += 1;
    }
    assert_eq!(checked, 8);
}
