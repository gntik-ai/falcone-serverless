# ctr-worker-execution-api: OCI profile v1

Contract: ctr-worker-execution-api (mode `oci` profile)
Version: 1

This profile defines OCI-04.t1 / FR-OCI-04 from the
[design](../../falcone-serverless-design.md) and
[ADR 0010](../../adr/0010-oci-execution-mode-and-base-catalog.md).
R1–R7 are stable rule IDs. Later incompatible changes require a new version.
This is an independently versioned profile, not a definition of the absent base
worker execution API or of `ctr-platform-service-identity`.

## Normative rules

| Rule | Requirement |
| --- | --- |
| R1 | The dispatcher-to-shim transport MUST be mutually authenticated TLS. Plaintext and server-only TLS MUST be rejected before reading any request (`request_read=false`). Only authenticated mutual TLS permits reading the request. |
| R2 | Before sending any execution credential or request, the dispatcher MUST verify the expected per-pod shim identity according to `ctr-platform-service-identity`. An absent, wrong or unverified identity, including a tenant squatting on the shim port, MUST refuse delivery, emit exactly one audit/security event per refused delivery, and leave `credential_sent=false` and `request_sent=false`. Only a verified expected shim identity allows delivery, with zero refusal events. |
| R3 | The shim MUST verify the dispatcher identity according to `ctr-platform-service-identity`. Missing, wrong or unverified identities MUST be rejected before any forwarding decision (`forwarded=false`). Verified identity permits acceptance; acceptance alone does not forward a request. |
| R4 | The per-pod shim identity volume MUST be mounted solely into the trusted shim container, never into the tenant, an init container or another sidecar. The pod MUST explicitly set `automountServiceAccountToken: false`. Missing, ambiguous or malformed descriptors, no shim mount, tenant mounts, and true or unset automount are non-conformant. This models only the identity-mount boundary, not the complete restricted-pod template. |
| R5 | The shim-to-container request MUST contain neither the execution credential field name nor its value anywhere in its serialized form, including nested fields, keys and strings. The credential stays in the shim. The model constructs a separate container request from `params` and `context` only, never copies the credential field, and refuses forwarding if either forbidden string survives. The actual localhost port and container envelope remain owned by `ctr-oci-function-contract`; this model does not redefine them. |
| R6 | `limit` (L) MUST be an integer >= 1. `in_flight` counts admitted, non-terminal attempts excluding the new one, and MUST be a nonnegative integer. Admit iff `in_flight < limit`; otherwise reject with `concurrency_limit_exceeded`. L=0 or malformed/missing limits are invalid configuration, never admission. This check neither reserves a slot nor implements synchronization. |
| R7 | The trusted deadline and observation time MUST be nonnegative integer milliseconds in the same clock domain. At admission, admit iff `now_ms < deadline_ms`; at equality or later reject with `deadline_exceeded`. For a response observed strictly before the deadline, report `completed`; at equality or later report `deadline_exceeded` and discard the result. Missing, malformed or unknown phase/time inputs MUST reject and discard. No production deadline source or default is specified. |

R1, R2, R3, R6 and R7 are cumulative gates: an isolated positive model verdict
never bypasses another gate. Unknown, missing and ambiguous inputs fail closed.
Identity states in fixtures are trusted verification results, not claims taken
from a request. `verified` means the expected identity has been authenticated,
including its pod/role binding. This profile defines neither certificate nor
projected-token validation: both identity forms permitted by ADR 0010 remain
subject to `ctr-platform-service-identity`, and neither removes R1's mTLS rule.
Fail closed until that dependency (SEC-01.t1, gntik-ai/falcone#1282) resolves.
Merge requires that predecessor, a second reviewer and operator approval (R3).

`concurrency_limit_exceeded` and `deadline_exceeded` are this profile's model
outcomes. The audit event count is normative; no Falcone audit event type or wire
schema is invented here. Falcone owns dispatcher implementation and audit storage.

## Fixture/model boundary

`fixtures-v1.json` contains hand-written cases with unique nonempty `id`, `rule`,
`input` and literal `expected` fields. Extra case fields, malformed cases, unknown
rules or outcomes, and missing positive/negative rule coverage fail validation.
Inputs are JSON plain data, intentionally including missing/unknown/wrongly typed
values to exercise fail-closed behavior. Unknown input keys also fail closed.

- R1 input: `transport` = `plaintext`, `server_tls` or `mutual_tls`.
- R2/R3 input: `identity` = `verified`, `absent`, `wrong` or `unverified`.
- R4 input: `identity_mounts` lists every identity-volume recipient by role; only
  `["shim"]` conforms. `automountServiceAccountToken` must be explicitly false.
  These roles are trusted descriptor observations, not tenant-selected labels.
- R5 input: nonempty synthetic `execution_credential`, plus `params` and `context`.
  Output is `{params, context}` or null on refusal. Forwarding is positive;
  rejection of a contaminated container request is negative. Matching checks
  decoded JSON keys/strings and their serialized representations (including
  substrings), conservatively refusing collisions rather than altering params.
- R6 input: `limit`, `in_flight`.
- R7 input: `phase` = `admission` or `response`, `now_ms`, `deadline_ms`.
  A positive response means timely observation only, not application success.

Results use accept/reject for R1/R3; allow/refuse, `audit_events`,
`credential_sent`, `request_sent` for R2; conformant/non_conformant for R4;
forward/refuse and a `request` for R5; admit/reject and `reason` for R6;
admit/reject/completed/deadline_exceeded, `reason` and `result_discarded` for R7.
`invalid_configuration` and `invalid_input` are model diagnostics, not new wire
error codes. R1/R3 additionally return `request_read`/`forwarded` respectively.
L=2 and deadline_ms=1000 are test values only.

The pure model is `worker/src/oci_execution_profile.rs`, declared `#[cfg(test)]`
only in the worker binary. Fixtures and source self-check are embedded with
`include_str!`; the model performs no runtime I/O, TLS or identity verification.
Passing tests prove agreement with these fixtures, **not runtime enforcement**.
No shim, listener, dispatcher, capability host, pod renderer, audit storage,
cancellation, progress/checkpoint/log relay or cluster adversarial test is added.
Later OCI-04 tasks and OCI-07 must prove real enforcement.
