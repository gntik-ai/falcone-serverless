//! WAS-02.t4: a guest header list is lifted lazily. A short list whose records all alias one
//! large guest string would copy that string once per record if the host lifted the whole list
//! before checking its size; the host must refuse it from the first element instead.
//!
//! The crate allows no hand-written allocator hooks, so the heap is not tracked directly; the
//! resident-set high-water mark of the process (`VmHWM`) is compared before and after the call
//! instead. Every record the host copied would be touched, so an eager lift shows as gigabytes.

use falcone_serverless_worker::host::egress::{Resolver, TcpConnector};
use falcone_serverless_worker::host::grant::GrantSet;
use falcone_serverless_worker::host::limits::HostLimits;
use falcone_serverless_worker::host::runtime::{Capabilities, Gate, InvocationContext};
use serde_json::json;
use std::io;
use std::net::IpAddr;
use std::sync::Arc;

/// Peak resident set of this process in bytes, from `/proc/self/status`.
fn peak_resident_bytes() -> Option<usize> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    let kib: usize = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

struct NoNames;

impl Resolver for NoNames {
    fn resolve(&self, _: &str) -> io::Result<Vec<IpAddr>> {
        Err(io::ErrorKind::NotFound.into())
    }
}

const MIB: usize = 1024 * 1024;
const HEADERS: usize = 256;
/// Each record names this many bytes. Lifted once per record, the call would copy 1 GiB.
const ALIASED_VALUE: usize = 4 * MIB;
/// Far under the 1 GiB an eager lift copies, far over one lifted string plus engine noise.
const PEAK_BUDGET: usize = 128 * MIB;

#[test]
fn aliased_header_strings_are_refused_before_they_are_copied() {
    let url = "http://allowed.example/";
    let wat = include_str!("../src/qualification/gate_fixtures/http-headers.wat")
        .replace("{{URL}}", url)
        .replace("{{URL_LEN}}", &url.len().to_string())
        .replace("{{COUNT}}", &HEADERS.to_string())
        .replace("{{NAME_LEN}}", "1")
        .replace("{{VALUE_LEN}}", &ALIASED_VALUE.to_string())
        .replace("{{PAGES}}", "96");
    let grants = GrantSet::from_manifest(&json!([{
        "family": "http",
        "interface": "falcone:host/http@0.1.0",
        "scope": {"hosts": ["allowed.example"]}
    }]))
    .unwrap();
    let caps = Capabilities {
        resolver: Some(Arc::new(NoNames)),
        connector: Arc::new(TcpConnector),
        ..Capabilities::default()
    };
    let gate = Gate::new(grants, caps, HostLimits::default()).unwrap();
    let component = gate.compile_wat(&wat).unwrap();
    let mut guest = gate
        .instantiate(&component, InvocationContext::default())
        .unwrap();
    let Some(before) = peak_resident_bytes() else {
        eprintln!("NOT RUN: no /proc/self/status on this platform, PR CI runs it");
        return;
    };
    // 0xE0000000 | 0x100 | 1: the http stage returned host-error limit-exceeded.
    assert_eq!(guest.call_u32("run").unwrap(), 0xE000_0101);
    let grown = peak_resident_bytes().unwrap().saturating_sub(before);
    assert!(
        grown < PEAK_BUDGET,
        "the call grew the resident peak by {grown} bytes"
    );
}
