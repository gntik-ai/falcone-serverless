//! WAS-02.t4: the verified invocation-context identifiers obey the host-to-guest ceiling of
//! `worker/wit/host.wit` (1 MiB of serialized bytes per call, the string frame included).

use falcone_serverless_worker::host::grant::GrantSet;
use falcone_serverless_worker::host::limits::HostLimits;
use falcone_serverless_worker::host::runtime::{Capabilities, Gate, InvocationContext};
use serde_json::json;

/// 1 MiB less the 8 bytes of pointer and length that frame a returned string.
const LARGEST_FITTING: usize = 1_048_576 - 8;

fn gate() -> Gate {
    let grants = GrantSet::from_manifest(&json!([])).unwrap();
    Gate::new(grants, Capabilities::default(), HostLimits::default()).unwrap()
}

fn context(which: &str, len: usize) -> InvocationContext {
    let mut context = InvocationContext::default();
    let value = "x".repeat(len);
    match which {
        "key" => context.invocation_key = value,
        "attempt" => context.attempt_id = value,
        _ => context.idempotency_key = value,
    }
    context
}

#[test]
fn identifiers_that_fit_the_ceiling_are_returned_whole() {
    let gate = gate();
    let component = gate
        .compile_wat(include_str!(
            "../src/qualification/gate_fixtures/invocation-context.wat"
        ))
        .unwrap();
    for (export, which) in [
        ("key", "key"),
        ("attempt", "attempt"),
        ("idempotency", "idempotency"),
    ] {
        let mut guest = gate
            .instantiate(&component, context(which, LARGEST_FITTING))
            .unwrap();
        assert_eq!(guest.call_u32(export).unwrap(), LARGEST_FITTING as u32);
    }
}

#[test]
fn an_identifier_over_the_ceiling_is_refused_before_the_guest_runs() {
    let gate = gate();
    let component = gate
        .compile_wat(include_str!(
            "../src/qualification/gate_fixtures/invocation-context.wat"
        ))
        .unwrap();
    for which in ["key", "attempt", "idempotency"] {
        for len in [LARGEST_FITTING + 1, 1_048_576] {
            let refused = gate.instantiate(&component, context(which, len));
            assert!(refused.is_err(), "{which} of {len} bytes was accepted");
        }
    }
}
