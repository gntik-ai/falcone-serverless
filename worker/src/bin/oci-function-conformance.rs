//! Standalone local conformance tool. Never called by the worker runtime.
#[path = "../oci_contract.rs"]
pub mod contract;

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    std::process::exit(contract::run(&args));
}
