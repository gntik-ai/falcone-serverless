# ctr-worker-execution-api v1

Contract: ctr-worker-execution-api
Version: 1

This is the Wasm-worker execution API of RUN-01 (FR-RUN-01, FR-RUN-02), the early provider of
`ctr-worker-execution-api` under ADR 0010 D1. It defines the messages and parsers
(`worker/src/api`) and, since RUN-01.t3, the HTTP service that serves them
(`worker/src/server`, started by `falcone-serverless-worker serve PATH`; see
[The HTTP service](#the-http-service)). The service reads no credential store. The dispatcher
adopts it in a later issue (INV-01). The [OCI profile](oci-profile.md) is an independent
profile of the same contract name for mode `oci`.

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
Every v1 code is `non_retryable`, including on every retry, except `execution_uncertain`, which
is `uncertain` (the outcome is not known and no state claims that external side effects were
reversed): a retried attempt of the same activation gets the same status, reason and class. No
v1 code is `retryable`. Later revisions add codes without changing these. The machine-readable table is [failure-codes.v1.json](failure-codes.v1.json).

| `code` | HTTP status | `detail.reason` | Class | Origin |
| --- | --- | --- | --- | --- |
| `GW_OCI_NOT_AUTHORIZED` | 403 | `oci_not_authorized` | non_retryable | ctr-functions-api v1 |
| `GW_BASE_DISABLED` | 409 | `base_disabled` | non_retryable | ctr-functions-api v1 |
| `GW_BASE_MISMATCH` | 409 | `base_mismatch` | non_retryable | ctr-functions-api v1 |
| `runtime_mode_not_supported` | 409 | `runtime_mode_not_wasm` | non_retryable | worker only |
| `invalid_request` | 400 | `missing_path` or `non_json_body` | non_retryable | worker only |
| `request_too_large` | 413 | `body_too_large` | non_retryable | worker only |
| `invalid_input` | 400 | `invalid_input` | non_retryable | worker only |
| `execution_cancelled` | 409 | `cancelled` | non_retryable | worker only |
| `deadline_exceeded` | 504 | `deadline_exceeded` | non_retryable | worker only |
| `guest_not_runnable` | 422 | `instantiation_failed`, `missing_handler_export`, `invalid_handler_signature`, `epoch_interruption_required` or `fuel_required` | non_retryable | worker only |
| `execution_failed` | 502 | `handler_failed`, `handler_trapped`, `invalid_handler_result`, `buffer_limit_exceeded`, `guest_memory_limit_exceeded` or `unclassified` | non_retryable | worker only |
| `execution_uncertain` | 502 | `uncertain` | uncertain | worker only |

The three `GW_` codes are referenced, not defined: their names, statuses and reasons are those of
the pinned error fixtures (below). ctr-functions-api v1 defines no other base error code. The
worker-only codes are exactly the last three: the non-wasm refusal, and the two refusals the
operator allowed for the legacy inputs below. Adding `GW_` equivalents to the functions API is a
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

## The HTTP service

`worker/src/server` serves the execution API on two pod-local TCP listeners with independent
capacity. A connection belongs to the listener it arrived on; the server never classifies a
connection after accept, so execute traffic cannot consume control capacity. Both listeners
serve one request per connection (`Connection: close`) over plain HTTP/1.1 inside the pod;
dispatcher authentication of either listener is decided with INV-01 and is not provided here.

| Listener | Config | Serves | Everything else |
| --- | --- | --- | --- |
| execute | `listen_port` (8080) | `POST /v1/execute` | `404` (`405` for another method on that path) |
| control | `control_port` (8082) | `POST /v1/cancel`, `POST /v1/update` | `404`, including `/v1/execute` |

The control transport is a second TCP port, not a Unix socket, so that the dispatcher can reach
it at the pod address on `control_port` (see [worker-runtime-config.md](../worker-runtime-config.md)).
The controller-written Services and the chart values do not expose `control_port` yet; that is a
follow-up in falcone-charts and in the controller.

### Capacity

- **Execute listener.** At most `concurrency` executions run at once. The listener admits at most
  `concurrency` + 4 connections (`ADMISSION_ALLOWANCE`, fixed): connections reading a request,
  writing a response or running a guest. A connection over that number is answered
  `503 {"error":"busy"}` at once and closed without its body being read. A request inside the
  allowance whose execution slot is taken is answered `503 busy` after it was parsed and judged
  (below). `concurrency` is between 1 and 256.
- **Control listener.** At most `control_max_connections` (16 by default, 1 to 256) connections;
  one more is answered `503 busy` at once. Its header and body deadlines are
  `control_header_deadline_ms` and `control_body_deadline_ms`. Stalled control connections never
  affect a running execution, and stalled execute connections never affect the control listener;
  the control listener recovers as soon as its deadlines have closed the stalled connections.

### Size ceilings and deadlines

| Limit | Value | Over the limit |
| --- | --- | --- |
| Header block (request line, headers and blank line), both listeners | 16 KiB | `431`, connection closed; no guest code runs |
| Request body, execute listener | 8 MiB (8388608 bytes) | `413 request_too_large` from the declared `Content-Length`; the body is not read |
| Request body, control listener | 64 KiB | `413 request_too_large` likewise |
| `__ow_body` (action body) | 1048576 bytes | `413`, the contract's `request_too_large` result; no guest code runs |
| Complete header block, execute listener | 5 s from accept, absolute | closed, no answer (a dripping connection is closed at the deadline) |
| Body, execute listener | 5 s per read and 30 s in total | closed, no answer |
| Header block and body, control listener | `control_header_deadline_ms`, `control_body_deadline_ms` | closed, no answer |
| Response write | 5 s | closed |

A request needs `Content-Length`; `Transfer-Encoding` is refused (`400`). After a response the
server half-closes and discards what the client still sends for at most 250 ms.

### Execute

The body is an `execute` message. The service decides in this order, and nothing later runs once
an earlier step answers:

1. A message that does not parse: `400 malformed_request`.
2. The contract refusals, passed through unchanged with their status and `failure` result:
   `runtime_mode` other than `wasm` (including an image-based revision and an unknown value)
   comes first, `409 runtime_mode_not_supported`, even when the worker is at capacity; then the
   legacy-input refusals (`400 invalid_request`, `413 request_too_large`). No instance is created.
3. `projection.engine_version` must be the pinned Wasmtime (`49.0.5`): otherwise
   `409 engine_version_mismatch`.
4. An execution slot: otherwise `503 busy`. A second execute for the same activation and attempt
   while one runs: `409 duplicate_attempt`. From here the execution is registered for cancel, so a
   cancel arriving while the request is still being admitted (scope intersection, compile) takes
   effect: the execute answers `409 cancelled`. Scope intersection uses hash sets, so its work is
   linear in the size of the grant lists.
5. The component. The worker reads the validated component the platform staged at
   `<cache_path>/artifacts/<hex>.wasm`, where `<hex>` is the digest of `projection.component_digest`
   and its bytes must hash to it (`424 artifact_unavailable`, `424 artifact_digest_mismatch`).
   How artifacts reach that directory (a fetch from `falcone_api_endpoint`) is not part of this
   task. A tenant-supplied precompiled artifact is never deserialized: the component is validated
   (`worker/src/artifact/validate.rs`) and compiled only by the trusted compiler, the
   `artifact-compile-child` binary the worker resolves beside its own executable. The production
   worker image (`worker/Containerfile`) therefore ships both binaries in `/usr/local/bin`; the
   offline test `worker/tests/production_image.rs` fails if it does not.
6. **Declared imports**, before anything is compiled or instantiated. Every import of the
   component must be in `projection.imports` **and** in `ALLOWED_IMPORTS`; an empty `imports`
   means none. Otherwise `422 {"error":"undeclared_import","import":"<name>"}` and no guest code
   runs. WASI imports (environment, filesystem, sockets) are never allowed, so a guest has no
   environment variables, preopened files or sockets.
7. The guest runs with only the `params` object as input. Its deadline is the smallest of
   `projection.budgets.timeout_ms`, `max_wall_clock_ms` and 30 s; fuel, host-call counts, the
   request `deadline` and host-call deadlines are limit enforcement and belong to RUN-02.

A success is `200` with the `result` message: the guest's output is the legacy handler result
(an object with `statusCode` is used as is and must satisfy the result schema; any other JSON
value is the body of a `200`), `logs` are the guest's log lines after redaction, and `identity` is
filled from the revision projection and the pinned Wasmtime version. A guest output that is not a
valid handler result is `502 invalid_handler_result`.

#### Failed executions

An execution that was admitted and did not succeed (a cancellation, a deadline, a trap, a handler
error, a guest that cannot be run as a handler, an invalid handler result, or an unknown outcome)
answers with the same `result` message as a success: `status` `failure`, the failure and its
`class` from the code table, the redacted `logs` the guest produced, the activation, attempt and
the `identity` of the pinned revision. The HTTP status is the failure's own status:

| Host outcome | `code` | HTTP status |
| --- | --- | --- |
| `invalid_input` | `invalid_input` | 400 |
| `cancelled` | `execution_cancelled` | 409 |
| `deadline_exceeded` | `deadline_exceeded` | 504 |
| `instantiation_failed`, `missing_handler_export`, `invalid_handler_signature`, `epoch_interruption_required`, `fuel_required` | `guest_not_runnable` | 422 |
| `handler_failed`, `handler_trapped`, `buffer_limit_exceeded`, `guest_memory_limit_exceeded`, a guest output that is not a handler result (`invalid_handler_result`) | `execution_failed` | 502 |
| `uncertain` | `execution_uncertain` | 502 |

A cancel that arrives before the guest starts (including during compilation) also answers
`execution_cancelled`.

The service-level errors above (`malformed_request`, `busy`, `undeclared_import`, `engine_version_mismatch`,
`duplicate_attempt`, `artifact_*`, `not_found`, `execution_not_found`) are
`{"error": <code>}` objects, not `result` messages: nothing of the revision ran and no
execution was attributed.

### Cancel and update (control listener)

`POST /v1/cancel` takes a `cancel` message in state `requested`, sets the execution's
cancellation, which interrupts the guest at its next epoch tick (5 ms) and refuses further host
calls, and waits for the execution to end for at most `cancel_reserve_ms` (5000 by default). It
answers `200` with the same message in state `acknowledged` once the execution has ended, and in
state `uncertain` when it has not ended within that time (for example a blocked host call, which
RUN-02's host-call deadlines will bound) or when a mutating host call may still commit. An
execution that is not running is `404 execution_not_found`. No state claims that external side
effects were reversed. The interrupted execute request answers `409 cancelled`.

`POST /v1/update` takes an `update` message for a running execution and answers `202` with
`{"accepted":true,"sequence":N}`. Sequences must increase: a repeated or older one is
`409 stale_sequence`. The worker keeps only the latest sequence; relaying progress and
checkpoints is RUN-02 and JOB work.

### Credential and redaction

The execution credential is parsed and checked against the request, then used for two things
only: the host `Context` (tenant, workspace, invocation identity) and the redaction set. It is
never passed to the guest, never placed in WASI environment or files, never written to a
worker log and never returned. The worker's structured events (one JSON object per line on stderr:
`listening`, `admitted`, `execution_started`, `execution_finished`, `panic`) carry only static
text and numbers, and the panic hook prints only a source line.

Guest log lines are the only text that leaves the host from guest code. Each execution redacts
them like this: keep at most 1000 lines; truncate each line to 8192 characters **first**; then
replace every string of the credential (all claim strings, grant scope entries, secret
references; one-character ones included) with `<redacted>` using one leftmost-longest
multi-pattern matcher (Aho-Corasick) built once per execution, so the work is linear in the
pattern bytes plus the line bytes; then truncate to 8192 characters again, because replacements
can lengthen a line. The work has a fixed budget of 12 MiB (credential-string bytes plus line
bytes). A credential string longer than any line can be (4 x 8192 bytes) cannot occur in a line and
is skipped: it costs no matcher and no budget. Over the budget, or if the matcher cannot be built, the logs are dropped and the result
carries the single line `<redacted>`: the failure mode never returns unredacted text. In a
release build, redacting 400,000 distinct strings against 1000 maximum-length lines takes under
one second.

## Qualification of the service

`worker/tests/execute_api_local.rs` starts the real `serve` binary and runs each case over
loopback TCP; the same cases (`worker/src/qualification/execution_api.rs`) run in the restricted
`fs-test-run01-*` pod through `worker/tests/cluster_execute_api.rs` (gated by `FS_TEST_CLUSTER`).
The cases, by acceptance criterion: `envelope` (identity), `runtime-mode`, `credential`,
`imports`, `redaction-load` (many credential strings and maximum log volume through the whole
request lifecycle, the fail-closed path, and a slot that is released),
`saturation` and `control-stall` (capacity, cancel under load, recovery),
`cross-listener`, and `headers`, `request`, `action-body`, `silent`, `drip`, `stalled-body`, `body-drip`
(every size ceiling and deadline; `body-drip` is a body that keeps arriving inside the idle
limit and is closed by the separate 30 s total body deadline; each is followed by a successful
execute and a successful cancel). Cases synchronize on the worker's events and on responses.

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
  the worker-only codes are exactly the three above;
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
