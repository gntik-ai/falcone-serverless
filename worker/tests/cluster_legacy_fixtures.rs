//! Operator-only JavaScript fixtures in the existing restricted namespace/pod harness.
#![cfg(target_os = "linux")]
#[path = "support/image.rs"]
pub mod image;
#[path = "support/kubeconfig.rs"]
pub mod kubeconfig;
#[path = "support/namespace.rs"]
pub mod namespace;

const QUALIFICATION_PREFIX: &str = "legacy-js";
// The fixture image installs compat_static under this harness's runner name. Only its two
// ignored component tests run; RUST_TEST_NOCAPTURE exposes their exact success record.
const QUALIFICATION_FLAG: &str = "--ignored";
const QUALIFICATION_CHECK: &str = "legacy_fixtures";
const CLUSTER_TEST: &str = "cluster_legacy_fixtures";
const SKIP_EXPLICIT_REQUEST_WITHOUT_GATE: bool = true;
const PRODUCTION_IMAGE_ENV: Option<&str> = None;

include!("support/was02_cluster.rs");

#[test]
#[ignore = "UNVERIFIED: FS_TEST_CLUSTER unset"]
fn cluster_legacy_fixtures() -> Result<(), String> {
    if std::env::var_os("FS_TEST_CLUSTER").is_none() {
        println!("UNVERIFIED: FS_TEST_CLUSTER unset; no client call");
        return Ok(());
    }
    live_qualification()
}

#[test]
#[ignore = "UNVERIFIED: trusted compiler image and FS_TOOLCHAIN_CACHE required"]
fn build_legacy_fixtures() {
    assert!(
        std::env::var_os("FS_TOOLCHAIN_CACHE").is_some(),
        "UNVERIFIED: FS_TOOLCHAIN_CACHE unset"
    );
    let output = std::env::var_os("FS_LEGACY_BUILD_OUTPUT")
        .expect("FS_LEGACY_BUILD_OUTPUT must name a new scratch directory");
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/legacy/build.py");
    assert!(
        std::process::Command::new("python3")
            .arg(script)
            .arg("--out")
            .arg(output)
            .status()
            .unwrap()
            .success()
    );
}
