# ctr-worker-execution-api v1

Contract: ctr-worker-execution-api
Version: 1

This is the Wasm-worker execution API of RUN-01 (FR-RUN-01, FR-RUN-02), the early provider of
`ctr-worker-execution-api` under ADR 0010 D1. The worker serves these messages on two pod-local HTTP listeners through
`falcone-serverless-worker serve CONFIG` (runtime configuration v2). The dispatcher integration
and authentication remain INV-01 work; this service reads no credential store. The
[OCI profile](oci-profile.md) is an independent profile of the same contract name for mode `oci`.

ADR 0006 (pool identity and the credential format) and ADR 0009 (Falcone authoritative for
revision and activation state) are Proposed. This contract follows the operator decisions
recorded in issue #124 and does not claim either ADR accepted.

## Messages and schemas

All messages are JSON objects with `version` (`"1"`) and `type`. Every object is closed
(`additionalProperties: false`) except the `params` object, which carries the action parameters.
The schemas are JSON Schema 2020-12 and reference one another by `$id`.

| Message | `type` | Schema |
| --- | --- | --- |
| Execute attempt (dispatcher to worker) | `execute` | [execute-request.v1.schema.json](execute-request.v1.schema.json) |
| Execution update (worker to dispatcher) | `update` | [update.v1.schema.json](update.v1.schema.json) |
| Cancel | `cancel` | [cancel.v1.schema.json](cancel.v1.schema.json) |
| Result (worker to dispatcher) | `result` | [result.v1.schema.json](result.v1.schema.json) |
| Revision projection (inside `execute`) | none | [projection.v1.schema.json](projection.v1.schema.json) |
| Shared definitions | none | [common.v1.schema.json](common.v1.schema.json) |

Later revisions may only add optional fields or new messages; a required field is a new major
version, because INV-01 (the mapping of ctr-revision-record onto the projection) and OCI-04
consume this shape.

### Execute

| Field | Meaning |
| --- | --- |
| `activation_id`, `attempt_id` | Identifiers, 1 to 128 characters of `A-Za-z0-9._:@/-`, starting alphanumeric. |
| `attempt_number` | Integer of at least 1. Retries share `activation_id` and record separate attempts. |
| `deadline` | Absolute UTC timestamp `YYYY-MM-DDTHH:MM:SS[.f]Z` (seconds 00 to 59). |
| `invocation` | `invocation_key`, optional `idempotency_key` and `correlation_id`: the invocation identity of `falcone:host/invocation-context`. `attempt-id` of that interface is `attempt_id`. |
| `credential` | The ctr-execution-credential v1 claims object, not an opaque string. Its schema is the vendored [claims schema](vendor/execution-credential.v1.claims.schema.json). Signing and transport encoding belong to that contract. |
| `projection` | The pinned revision projection below. |
| `timeout_ms` | Optional positive guest wall-clock limit in milliseconds; default 30000. Explicit null is malformed. |
| `params` | The legacy `main(params)` object: `__ow_*` request fields beside the action parameters. |

The credential must be bound to the request: `credential.activation`, `credential.attempt` and
`credential.revision` equal `activation_id`, `attempt_id` and `projection.revision`. A mismatch is
malformed. The credential never reaches a guest.

### Revision projection

| Field | Meaning |
| --- | --- |
| `revision` | The pinned function revision identifier. |
| `runtime_mode` | Required. Enum `wasm` or `oci`. |
| `base` | Tagged union, or absent when the revision pins no base. |
| `artifact_digest` | Digest of the raw artifact. |
| `component_digest` | Digest of the validated component. |
| `language_profile` | `{name, version}`, an exact profile pin. |
| `engine_version`, `wasi_version`, `interface_version` | The engine (Wasmtime), WASI and host interface versions. `interface_version` is `0.2.0`, from `falcone:host@0.2.0`. |
| `imports` | Declared imports; empty means none. |
| `budgets` | Integers from 1 to 2^53 - 1: `fuel`, `memory_bytes`, `timeout_ms`, `max_host_calls`. |
| `grants` | Granted capabilities, validated as the revision-runtime-manifest grants (`@0.2.0`). |

Digests are `sha256:` followed by 64 lowercase hex digits.

`base` is one of:

- `{kind: "catalog", id, version, digest}`: a catalog base resolved to its `id@version` and digest.
  `id` is lowercase `a-z0-9._-`; `version` is a concrete `MAJOR.MINOR.PATCH`.
- `{kind: "byo", image_digest, signer_identity, attestation_ref}`: a bring-your-own image.

An unresolved default alias (`default`, `latest`, a `kind` of `alias`, any other `kind`, or any
field not listed) is malformed: Falcone resolves aliases at deploy and pins the result. Absent
means the revision pins no base; `base: null` is malformed.

The Wasm worker admits only `runtime_mode` `wasm`; `oci` is a valid projection value that the Wasm
worker refuses (below). OCI-only projection fields are added by OCI-04, additively.

### Update

`update` carries `activation_id`, `attempt_id`, a `sequence` of at least 1 and an `update` object
discriminated by `kind`:

| `kind` | Fields |
| --- | --- |
| `heartbeat` | none |
| `progress` | `completed` (integer, 0 to 2^53 - 1) and optional `total` (at least 1, at least `completed`) |
| `checkpoint` | `checkpoint_ref`: a reference to a committed checkpoint object, never its payload |

### Cancel

`cancel` carries `activation_id`, `attempt_id` and `cancel.state`: `requested` (dispatcher to
worker), `acknowledged` or `uncertain` (worker to dispatcher). `uncertain` means the outcome of the
cancellation is not known; no state claims that external side effects were reversed.

### Result

The result keeps the legacy `{status, result, logs}` envelope and adds `activation_id`,
`attempt_id`, `attempt_number` and the required `identity` block.

- `status` is `success` or `failure`.
- On `success`, `result` is the legacy handler result `{statusCode, headers?, body?}`: an integer
  `statusCode` from 100 to 599, optional string `headers`, and optional `body` (a JSON value that
  is not `null`; a 304 has none, and a 304 result carrying `body` is malformed). `class` is absent.
- On `failure`, `result` is the failure envelope below and `class` is required.
- `logs` is an array of at most 1000 strings of at most 8192 characters.

Every integer field is an integral JSON number between its minimum and 2^53 - 1 (9007199254740991); `1` and `1.0` are the same integer, and fractions and larger values are malformed.

An optional field specified as absent is never `null`: `base` in the projection, `base_digest` in
the identity, `class`, `headers`, `body`, `idempotency_key`, `correlation_id`, `total`,
`secretRef` and `guestGrantId` reject an explicit `null`. The runtime parsers read these fields
with the `present` deserializer, which distinguishes absent, `null` and a value.

#### Activation runtime identity

Every result carries `identity`: `function_revision`, `runtime_mode`, `artifact_digest`,
`base_digest` (present exactly when the revision pins a base), `wasmtime_version`,
`language_profile` and `interface_version`. `parse_result` takes the request, or the projection,
that the result answers and rejects a result whose identity does not describe it. In particular a
pinned base without `base_digest`, a `base_digest` without a pinned base, and a `base_digest` of
`null` in either case are all rejected. The base digest is `digest` of a catalog base and
`image_digest` of a BYO base. A result parsed against a request must also repeat its
`activation_id`, `attempt_id` and `attempt_number`.

## Failure codes and classes

A failure `result` is `{status, code, message, detail: {reason}, retryable}`, shaped as the
ErrorResponse of ctr-functions-api v1 without the request-correlation fields. The class is one of
`retryable`, `non_retryable` and `uncertain`; `retryable` is true exactly for class `retryable`.
Every v1 code is `non_retryable`, including on every retry: a retried attempt of the same
activation gets the same status, reason and class. Later revisions add codes of the other classes
without changing these. The machine-readable table is [failure-codes.v1.json](failure-codes.v1.json).

| `code` | HTTP status | `detail.reason` | Class | Origin |
| --- | --- | --- | --- | --- |
| `GW_OCI_NOT_AUTHORIZED` | 403 | `oci_not_authorized` | non_retryable | ctr-functions-api v1 |
| `GW_BASE_DISABLED` | 409 | `base_disabled` | non_retryable | ctr-functions-api v1 |
| `GW_BASE_MISMATCH` | 409 | `base_mismatch` | non_retryable | ctr-functions-api v1 |
| `runtime_mode_not_supported` | 409 | `runtime_mode_not_wasm` | non_retryable | worker only |
| `invalid_request` | 400 | `missing_path`, `non_json_body` or `malformed_request` | non_retryable | worker only |
| `request_too_large` | 413 | `body_too_large` | non_retryable | worker only |
| `timeout` | 504 | `execution_timeout` | non_retryable | worker service |
| `busy` | 503 | `capacity_exhausted` | non_retryable | worker service |
| `undeclared_import` | 422 | `undeclared_import` | non_retryable | worker service |
| `cancelled` | 499 | `cancelled` | non_retryable | worker service |
| `execution_failed` | 500 | `execution_failed` | non_retryable | worker service |
| `not_found` | 404 | `not_found` | non_retryable | worker service |
| `stale_update` | 409 | `stale_update` | non_retryable | worker service |

The three `GW_` codes are referenced, not defined: their names, statuses and reasons are those of
the pinned error fixtures (below). ctr-functions-api v1 defines no other base error code. The original three worker-only admission codes remain unchanged. The worker-service
codes add bounded service admission and execution outcomes; `busy` is an attempt refusal,
and retry policy remains with Falcone. In v1 `retryable: false` describes this
attempt; it does not authorize worker retries or prohibit Falcone from admitting
a new attempt after capacity recovers. Adding `GW_` equivalents to the functions API is a
follow-up. A request this worker refuses carries no request value in its message.

### The non-wasm refusal

The Wasm worker refuses any execute message whose `runtime_mode` is not `wasm` with
`runtime_mode_not_supported` (409, reason `runtime_mode_not_wasm`, non-retryable), before it checks
anything else and before anything is invoked. This includes a value the schema does not know.
`parse_projection` accepts both modes: admission, not parsing, makes the refusal.

### Legacy inputs rejected before invocation

The frozen inventory (`docs/contracts/falcone-contract-inventory.md`, legacy envelope fixtures)
requires three legacy inputs to be rejected before invocation. This contract maps them:

| Input | Code | Status | `detail.reason` | Retryable |
| --- | --- | --- | --- | --- |
| `missing-path`: `params` without `__ow_path` | `invalid_request` | 400 | `missing_path` | false |
| `non-JSON-body`: a JSON content type with an unparsable `__ow_body` | `invalid_request` | 400 | `non_json_body` | false |
| `oversized-body`: `__ow_body` longer than 1048576 bytes | `request_too_large` | 413 | `body_too_large` | false |

The checks run in this order: path, size, JSON. The size is `max-host-to-guest-bytes` of
`worker/wit/host.wit`. The content type is the `content-type` entry of `__ow_headers`, compared
case-insensitively with `application/json` ignoring parameters. The fixture labels of the inventory
(`envelope-invalid`, `body-not-json`, `body-too-large`) are not wire codes; this mapping is. The
legacy handler is not invoked for any of them.

## Grant versions

The credential's `allowed_capabilities` keeps the canonical capability-grants v1 reference (`falcone:host/*@0.1.0`), while the request's `projection.grants` validate against the revision-runtime-manifest grant definitions at `@0.2.0`; bumping capability-grants to 0.2.0 is a follow-up in Falcone and this contract changes nothing there.

The two lists are validated separately and never mixed: a credential with a `0.2.0` grant fails, and
a projection with a `0.1.0` grant fails. The credential's `$ref`
(`https://falcone.dev/contracts/serverless/capability-grants/v1#/$defs/grant`) is kept exactly as
published and is resolved offline from [vendor/](vendor/README.md).

## Runtime parsers and redaction

`worker/src/api` has typed serde structs and the entry points `parse_request`, `parse_projection`,
`parse_update`, `parse_cancel` and `parse_result(result, request_or_projection)`. They enforce the
schemas with serde and explicit checks and never load or evaluate JSON Schema (`jsonschema` is a
dev-dependency). Every rule is compiled in every build.

Redaction by construction: no type in `worker/src/api` derives `Debug`. Each type that can hold a
request value implements `Debug` and `Display` by hand and prints `<redacted>` for it; plain enums
print only variant names. Parse errors name the violated rule and never quote the input. The
serialized refusal keeps the contract fields above; only formatted error text is redacted.

## Conformance

`worker/tests/execution_api_contract.rs` runs under `cargo test --workspace --locked` and executes
no guest code and no network access:

- every fixture under `worker/tests/fixtures/contract/` is validated with the `jsonschema` crate
  against the complete schemas, with the vendored schemas registered offline, and the runtime
  parser must agree: a fixture the schema rejects is rejected by the matching `parse_*` function,
  and one it accepts is accepted. `result/cross/`, `request/rejected/` and `update/rejected/` hold
  documents that a schema alone cannot judge (cross-document or cross-field rules); the schema
  accepts them and the parser rejects them;
- the vendored schemas and the pinned functions-api error fixtures have their sha256 values pinned;
- the failure code table is compared with the pinned functions-api error fixtures: every worker
  code of origin `functions-api` appears there with the same status, reason and retryability, and
  the original worker-only codes remain exactly the three admission codes above;
- the legacy fixtures of the inventory are used byte for byte, with sha256 values equal to the
  inventory, and valid ones round-trip through the worker types unchanged.

## Provenance

All sources are in gntik-ai/falcone, public. The maker sandbox cannot read that repository, so the
pinned excerpts were supplied by the operator (issue #124, 2026-11-08 and 2026-11-11).

| Source | Revision | sha256 | Use |
| --- | --- | --- | --- |
| docs/contracts/serverless/functions-api/openapi.v1.yaml | a19f40ab9e160cd522c110efd50c30b2f5e0d708 | 03121286b0c7ec716304d51a41b866477f4fc21671d95ebeafb8bf9da70e8960 | ctr-functions-api v1: error envelope, provisioning failure reasons |
| tests/contracts/functions-api/fixtures/errors/base_disabled.json | a19f40ab9e160cd522c110efd50c30b2f5e0d708 | 9ed46a38dca56af8f9b9fdc16a413fec45f48a4b1324de2d9a400ed31c49c7d8 | pinned copy in `worker/tests/fixtures/contract/functions-api/errors/` |
| tests/contracts/functions-api/fixtures/errors/base_mismatch.json | a19f40ab9e160cd522c110efd50c30b2f5e0d708 | 2ac0fa4c05c628d6a251657de9918564652d32af9473502de510eaedfdcfa92c | same |
| tests/contracts/functions-api/fixtures/errors/oci_not_authorized.json | a19f40ab9e160cd522c110efd50c30b2f5e0d708 | ef0763c3f590a7e5fb31b59073e240d5320681b0f70f13757327cdb7bb26d740 | same |
| docs/contracts/serverless/execution-credential/execution-credential.v1.claims.schema.json | a19f40ab9e160cd522c110efd50c30b2f5e0d708 | 9c98ba16548e724004fbf17c38d3d689fd7183d25b386c83e61b13ff8b31ecf0 | vendored claims schema (ctr-execution-credential v1) |
| docs/contracts/serverless/capability-grants/capability-grants.v1.schema.json | a19f40ab | 7f93939d0cc4b87d0bb6ef9af145a8b68a060ab918dc96ffc6247086a2b25dd2 | vendored grant definitions the claims reference |

The legacy envelope and invocation-identity fixtures are those of
`docs/contracts/falcone-contract-inventory.md` (MIG-01.t3, #278); their sources and hashes are
recorded there. This task does not edit the inventory.


## Pod-local service (RUN-01.t3)

RuntimeConfig v2 configures `listen_port` (execute, default 8080) and the required,
distinct `control_port` (example 8082). Both bind **127.0.0.1**, inside the pod network
namespace, with no host-network listener. The dispatcher uses the pod-local control
address `127.0.0.1:<control_port>` through its future INV-01 transport integration.
Remote dispatcher reachability and authentication are not implemented or qualified here.
Runtime-config v1 parsing stays closed and cannot enable this service.

| Listener | Operations | Fixed connection ceiling |
| --- | --- | --- |
| Execute | `POST /v1/execute` only | concurrency + 4 admission connections |
| Control | `POST /v1/cancel`, `POST /v1/update` only | 8, independent of execute |

Each listener returns 404 for the other's operations. Connections carry one request;
there is no keep-alive, pipelining, transfer encoding or content encoding. At most
`concurrency` execution permits exist, including execute body transfer and response
writing. A complete execute header gets 503 `busy` before reading its body when
execution permits are occupied. Overflow connections beyond the listener ceiling
receive an immediate best-effort nonblocking 503 and close, without parsing or
allocating a connection thread. Existing connections are never evicted. Accept errors (including descriptor or memory
exhaustion and aborted connections) emit value-free listener/error-kind diagnostics
and retry after a fixed 50 ms backoff. Neither listener silently exits on an accept
error; recovery requires the external resource condition to clear.

For refusal precedence before a body can be admitted, the dispatcher supplies
`X-Falcone-Runtime-Mode` with the projection's mode (and `X-Falcone-Image` for an
image reference). An arrived non-wasm selector gets 409 before capacity, size or
credential checks. The JSON projection is independently checked before every
other request check, including unknown and non-string modes and image references.
Connections refused before any headers arrive cannot have body-dependent precedence.
An optional absent runtime header never authorizes a non-wasm JSON projection.

| Transfer bound (both listeners) | Value |
| --- | --- |
| Headers, including request line and terminator | 16 KiB |
| Execute JSON request | 8 MiB |
| Control JSON request | 16 KiB |
| Legacy action body (`__ow_body`) | 1 MiB UTF-8 bytes |
| Encoded legacy params, including metadata (existing WIT ceiling) | 1 MiB |
| Absolute header deadline (also idle connection) | 5 s |
| Body inactivity deadline | 2 s |
| Absolute body transfer deadline | 10 s |
| Absolute response write deadline, header plus body | 5 s |
| Requested per-socket send buffer (kernel may double it) | 64 KiB |
| Cancellation observation window | 1 s |
| Qualification scheduling/transport tolerance | 2 s |

Oversized requests receive 413; malformed or oversized headers and expired transfers
may close without a response. Each partial read is limited by the remaining absolute
budget. Response writes use **one Instant deadline**, recomputed before each partial
write across both header and body; progress never resets it. Deadline expiry closes
the socket and drops both connection and execution permits. A stalled connection
cannot retain a thread past its transfer deadline plus the 2 s qualification tolerance.
Cancellation returns `acknowledged` only after the guest reports cancellation;
unknown, raced or unobserved completion is `uncertain`, without claiming reversal
of effects. Updates are validated heartbeat/progress/checkpoint-reference messages
for a currently registered attempt, with increasing sequence numbers; unknown
attempts get 404 and stale sequences get 409. No update mutates platform state.

The platform stages **raw validated components** at
`<cache_path>/components/<64-lowercase-hex-component-digest>`. The service reads at
most 64 MiB, verifies the component digest and validates imports before asking the
existing supervised trusted compiler/cache to compile it. No HTTP request selects a
filesystem path, compiler binary or native cache. Every import must be in both
`projection.imports` (empty means none) and `ALLOWED_IMPORTS`, otherwise the result
is 422 `undeclared_import` before instantiation, even on a compile-cache hit.
The pinned engine must match 49.0.5. Identity uses the projection's artifact digest,
revision, base, profile and interface, and the worker's pinned Wasmtime version.
The host retains the credential; params and invocation context are the only guest
input. No WASI environment, preopens or credential-bearing memory exports are linked.
External capability bindings fail closed pending their platform integrations.

The effective guest timeout is **min(request timeout_ms, projection budgets.timeout_ms,
worker wall clock)**. The worker wall clock is min(RuntimeConfig default_wall_clock_ms,
max_wall_clock_ms, the existing engine's 30000 ms hard ceiling). Trusted validation and
compilation have their separate, existing 300 s supervised budget. A single
process-owned trusted cache serializes compilation and cache lookup; a cold compile
can delay other requests, including cache hits, while they hold execution permits.
The queue wait precedes the guest clock and is not charged to timeout_ms; a cancel
in that phase can report uncertain. This is separate from the qualified guest timeout;
the guest clock starts at preparation/instantiation, before any guest start code. Expiry returns
HTTP 504 with `timeout`, reason `execution_timeout`, activation and attempt attribution
and runtime identity, and releases capacity. Tests warm compilation outside the
measured guest invocation and require 1000 ms to finish in [900, 3000) ms. Blocked
host-call deadline enforcement and RUN-02 lifecycle work remain outside this change.

Structured log lines are truncated to 8192 characters **before** redaction. One
byte-alphabet Aho-Corasick automaton covers raw and JSON-escaped credential strings,
including one-character secrets. The union of overlapping matches prevents a suffix
from leaking behind a shorter match; a secret prefix cut by truncation is covered.
Building costs O(256 * secret bytes), scanning O(log bytes). Per execution the fixed
bounds are 262144 serialized credential bytes, 16384 states and 33554432 work units
(including construction, traversal and truncated text). Exceeding any bound drops
all logs and returns exactly `["<redacted>"]`. At most 1000 output lines are retained.
Qualification covers both the 8 MiB early fail-closed case and a matcher-path case
with 15 shared-prefix secrets of lengths 15900..15914, under 256 KiB serialized
credentials and 16384 states, and 1000 8192-character lines (14192 bytes each,
over 32 million work units including construction). The latter must return 1000
redacted lines, proving that it did not take a budget fallback. Each case must
redact within 2 s (debug and release); full service qualification also asserts slot recovery.
No dependency was added for the matcher. The already pinned rustix 1.1.5 enables
its `net` feature to bound send queues and to set a 16 KiB receive buffer for
slow-reader qualification; this avoids depending on host TCP autotuning defaults.

`worker/tests/execute_api_local.rs` exercises the public admission/redaction seams and
real TCP service behavior. `worker/tests/cluster_execute_api.rs` runs matching cases
in `fs-test-run01-<run-id>` with the restricted pod harness when `FS_TEST_CLUSTER=1`,
using the pinned `FS_TEST_WAS02_IMAGE`, `KUBECONFIG`, and `FS_TEST_EVIDENCE_DIR`.
Create mode creates/deletes its dedicated namespace; shared mode uses only run-owned
objects and never creates/deletes a namespace. Cases include identity, refusals,
credential isolation, all four declared-import combinations, saturation, control
recovery, every size/transfer bound, slow readers and each timeout precedence variant.
Retained evidence contains only allow-listed case records, namespace/run/image, and
an artifact ID with SHA256. An unset cluster gate explicitly reports UNVERIFIED.
The PR must record three consecutive green Rust CI runs of its head, listing and
excluding only the separately tracked compiler launcher flake. This source change
claims neither those runs nor live cluster qualification or ADR 0006 acceptance.
