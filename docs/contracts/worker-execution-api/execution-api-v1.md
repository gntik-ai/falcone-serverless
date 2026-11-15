# ctr-worker-execution-api v1

Contract: ctr-worker-execution-api
Version: 1

This is the Wasm-worker execution API of RUN-01 (FR-RUN-01, FR-RUN-02), the early provider of
`ctr-worker-execution-api` under ADR 0010 D1. `worker/src/api` defines the messages and their
parsers; `worker/src/server` serves them over HTTP on two pod-local listeners (see
[Transport](#transport-two-pod-local-listeners)) and drives the engine harness. How the dispatcher
authenticates to either listener is not decided here: it is recorded with INV-01
(gntik-ai/falcone), and until then the listeners are reachable only inside the pod network and
pools stay at zero replicas. The [OCI profile](oci-profile.md) is an independent profile of the
same contract name for mode `oci`.

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
A code has one class, except where the table says a reason has its own. A retried attempt of
the same activation gets the same status, reason and class. The machine-readable table is
[failure-codes.v1.json](failure-codes.v1.json), and `worker/src/api/refusal.rs` (`Code`, `Reason`),
`common.v1.schema.json` and this table must agree: `worker/tests/execute_api_local.rs` checks it.

| `code` | HTTP status | `detail.reason` | Class | Origin |
| --- | --- | --- | --- | --- |
| `GW_OCI_NOT_AUTHORIZED` | 403 | `oci_not_authorized` | non_retryable | ctr-functions-api v1 |
| `GW_BASE_DISABLED` | 409 | `base_disabled` | non_retryable | ctr-functions-api v1 |
| `GW_BASE_MISMATCH` | 409 | `base_mismatch` | non_retryable | ctr-functions-api v1 |
| `runtime_mode_not_supported` | 409 | `runtime_mode_not_wasm` | non_retryable | worker only |
| `invalid_request` | 400 | `missing_path`, `non_json_body`, `malformed_http`, `malformed_message`, `budget_above_worker_ceiling` or `engine_version_mismatch` | non_retryable | worker only |
| `request_too_large` | 413 | `body_too_large` or `headers_too_large` | non_retryable | worker only |
| `execution_failed` | 500 | `handler_failed`, `invalid_input`, `invalid_handler_result`, `guest_trapped`, `instantiation_failed`, `artifact_rejected`, `artifact_unavailable`, `memory_budget_exceeded`, `fuel_exhausted`, `host_call_budget_exceeded`, `buffer_limit_exceeded`, `uncertain_side_effects` or `internal_error` | non_retryable; `uncertain_side_effects` is uncertain | worker only |
| `timeout` | 504 | `deadline_exceeded` | uncertain | worker only |
| `cancelled` | 499 | `cancel_requested` | uncertain | worker only |
| `busy` | 503 | `at_capacity` | retryable | worker only |
| `undeclared_import` | 422 | `import_not_declared` | non_retryable | worker only |
| `not_found` | 404 | `unknown_operation` or `unknown_execution` | non_retryable | worker only |
| `stale_update` | 409 | `sequence_not_newer` | non_retryable | worker only |

The three `GW_` codes are referenced, not defined: their names, statuses and reasons are those of
the pinned error fixtures (below). ctr-functions-api v1 defines no other base error code. This
change defines every other code in the table, and no code beyond it: the budget outcomes are
`execution_failed` with a distinct `detail.reason`, not codes of their own. `GW_*` codes are
unchanged. Adding `GW_` equivalents to the functions API is a follow-up. A request this worker
refuses carries no request value in its message.

`timeout` and `cancelled` are `uncertain` because a guest may have made a side effect before it
ended. `busy` is the only `retryable` code: nothing ran.

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

## Transport: two pod-local listeners

Both listeners are configured in `RuntimeConfig` (`worker/src/config.rs`) and speak HTTP/1.1 with
one request per connection, `Content-Length` bodies only, and JSON responses.

| Listener | Config | Serves only | Refuses |
| --- | --- | --- | --- |
| execute | `listen_port` (default 8080) | `POST /v1/execute` | everything else, `404 not_found` (`unknown_operation`) |
| control | `control_port` (default 8082), a second pod-local port | `POST /v1/cancel` and `POST /v1/update` | everything else, including execute, `404 not_found` (`unknown_operation`) |

The dispatcher uses `<pod address>:control_port` for control. Each listener has its own fixed
connection limit and its own header, body, idle and response deadlines, and nothing moves capacity
between them, so no execute traffic can consume control capacity and no connection is evicted.

| Limit | Execute listener | Control listener |
| --- | --- | --- |
| Connections | `concurrency` running executions plus 4 in admission | 16 |
| Request line and headers | 16 KiB, within 5 s | 16 KiB, within 2 s |
| Body | 8 MiB, within 30 s | 64 KiB, within 2 s |
| Longest wait for the next byte | 5 s | 1 s |
| Whole response write | 5 s | 2 s |

Every deadline is absolute from the start of its phase: a peer that drips bytes cannot extend it,
and a client that reads a large response one byte at a time is cut off when the response deadline
passes. A connection over the listener's limit is answered `503 busy` at once, with nothing read.
A header or body over its limit is refused (`413 request_too_large`, reason `headers_too_large` or
`body_too_large`); a connection that sends nothing, drips, or stalls is closed at its deadline.
Closing releases the connection thread and its permit at once.

### Admission order on the execute listener

1. A request that is not `POST /v1/execute` is `404`. A body longer than 8 MiB is `413`.
2. With every execution slot busy, the body is not waited for. Only bytes that already arrived
   with the headers (or are already readable without waiting) are looked at: a runtime mode other
   than `wasm` is still `409 runtime_mode_not_supported`, including when the complete body is
   already buffered, and any other request is `503 busy`.
3. Otherwise the body is read and parsed. A `runtime_mode` other than `wasm` is
   `409 runtime_mode_not_supported` and nothing is created. A projection naming an OCI image is a
   non-`wasm` runtime mode. Malformed messages are `400 invalid_request`.

### Cancel and update (control listener)

`POST /v1/cancel` takes a `cancel` message in state `requested`. For a running attempt it sets
the execution's cancel flag and waits up to the termination budget (500 ms) for the execution to
end, then answers `200` with the `cancel` message in state `acknowledged` (it ended) or `uncertain`
(it has not, for example because it is blocked in a host call that cannot be interrupted; RUN-02
closes that gap). An attempt that is not running is `404 not_found` (`unknown_execution`).

`POST /v1/update` takes an `update` message for a running attempt and answers `202` with the same
message. Sequences only move forward: a `sequence` that is not greater than the last accepted is
`409 stale_update`. Updates carry no payload the worker acts on in this version.

## Execution

An execute request names its artifact by `projection.artifact_digest`. The worker's platform
supplies the bytes (this version reads `<artifact_path>/<hex digest>.wasm` from a pod-local
directory; how the dispatcher delivers artifacts is recorded with INV-01) and the worker checks
the digest. Nothing a tenant supplies is ever deserialized as native code: the artifact is
validated once, compiled by the trusted compile child, and the compiled output is loaded only
after its integrity and provenance are verified.

- **Runtime identity.** Every result carries the `identity` block of the projection and the
  pinned Wasmtime version. A projection whose `engine_version` is not the worker's pinned
  Wasmtime is refused `400 invalid_request` (`engine_version_mismatch`).
- **Handler result.** The guest returns JSON. An object with `statusCode` is the legacy
  `{statusCode, headers?, body?}` result; any other JSON value is the `body` of a 200. Anything
  else is `execution_failed` (`invalid_handler_result`).
- **Declared imports.** Every import of the component must be declared in `projection.imports`
  and be one of the worker's allowed interfaces; otherwise `422 undeclared_import` before any
  guest code runs. An empty list means no import.
- **Grants.** The capability host gets the scopes that both the projection's grants and the
  credential's `allowed_capabilities` name.

### One absolute deadline

`until` is computed once, at admission, as the shortest of the request's own `deadline`, the
projection's `budgets.timeout_ms` and the worker's `max_wall_clock_ms`. Nothing after admission
derives a deadline from a duration:

- every supervised step (artifact fetch, validation, the cache operation, binding the
  credential) is left at `until` or on cancel, and its result is then not used;
- the guest's epoch deadline is `until`, and no guest starts at or after `until` (checked after
  instantiation and before the handler is invoked);
- each host call's timeout is the shorter of the host-call ceiling and `until - now`, and a call
  that starts at or after `until` is refused before it reaches the host and ends the execution
  as `504 timeout`; the deadline is checked before the call is counted, so it also wins over
  `host_call_budget_exceeded`.

When `until` passes or a cancel arrives during setup, binding, validation or a cache operation, the
execution slot is released within the termination budget (500 ms) and the outcome is `504 timeout`
or `499 cancelled`, whatever the step later returns. A supervised step that was left is
abandoned: it is told to stop where it can observe that, it never publishes a cache entry (the
cache writes a temporary file and renames it atomically, and discards it when abandoned), and at
most `max(concurrency, 4)` abandoned steps may still be running; past that a new step is
`503 busy`. A deadline or cancel always outranks `busy` and every admission result, including the
refusal of a duplicate registration: a request for an `(activation_id, attempt_id)` that is already
running is `503 busy` before `until` and `504 timeout` once it has passed.

A compile child that the supervisor stopped is classified by the reason it was stopped, recorded
before the kill: `compile_deadline_exceeded` for the deadline and `compile_cancelled` for a cancel,
whatever status or signal the killed child reports. The stop flag a supervised step receives is
set only for a cancel; a step that was left at `until` observes the deadline itself, so a child
stopped at the deadline is never reported as `compile_cancelled`.

### Budgets

The revision's budgets are limits, not observations:

| Budget | Enforced as | Outcome when breached |
| --- | --- | --- |
| `memory_bytes` | guest memory limit: the minimum of the budget, the worker's `max_memory_bytes` and the compatibility ceiling (256 MiB); a budget above the minimum of the last two is refused before compile or instantiate | `execution_failed`, `memory_budget_exceeded` |
| `fuel` | the store's fuel; running out ends the execution | `execution_failed`, `fuel_exhausted` |
| `max_host_calls` | every guest-to-host entry counts, including storage open and drop callbacks and calls that authorization refuses; the call after the budget is refused before the host | `execution_failed`, `host_call_budget_exceeded` |
| `timeout_ms` | part of `until` | `504 timeout` |

A budget the worker cannot honour is refused `400 invalid_request` with reason
`budget_above_worker_ceiling`; it is never replaced by a default. Calls already made are not
rolled back.

## The execution credential and logs

The credential is bound in the host binding layer (`worker/src/server/platform.rs`) and nowhere
else. The guest receives no environment, no preopened file and no memory export holding it, and
the worker never formats it. Log lines that reach a result are redacted on every exit path:

- the secrets are the claim strings of the credential (`iss`, `aud`, `tenant`, `workspace`,
  `pool_profile`, `revision`, `activation`, `attempt` and every scope and secret name of its
  grants);
- no output line contains eight or more consecutive characters of a secret, or a whole secret
  shorter than eight characters;
- text is redacted in full and only then cut to 8192 characters, so a secret that is longer than
  the cut, crosses it, or ends exactly at it is gone before the cut; each replaced run is the
  marker `<redacted>`;
- redaction is bounded: the matcher holds at most 2^20 secret windows and one execution scans at
  most 4 Mi characters. Building the matcher is bounded while it runs, not after: the cap on
  windows is checked as each window is inserted, a build examines at most 4 Mi windows
  (duplicates included), and it ends at the execution's absolute deadline `until`. Past any bound
  the execution fails closed: every log line is dropped and `logs` is the single line
  `<redacted>`.

## Test-only hooks

Delay and stall hooks, validation counters and their accessors exist only under `cfg(test)` or the
`qualification-hooks` feature of the worker crate and are absent from release builds.

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

## Service conformance

`worker/tests/execute_api_local.rs` runs a real server on loopback sockets, with guests compiled
from WAT and a test platform at the system boundary. It covers identity, the refusals and their
admission order, declared imports, credential absence and redaction (including the cut point and
the over-budget marker), saturation with stalled connections on both listeners, the listener
refusal matrix, timeout selection, every budget, supervised validation and cache stalls under
`until` and cancel (measured against the 500 ms termination budget), HTTP ceilings, transfer and
response deadlines with slot recovery, the production binary and image layout, and that this
document, `failure-codes.v1.json`, `common.v1.schema.json` and `refusal.rs` agree on every code.
Tests synchronize on server-side events (guest start, setup and cache stages, slot release), never
on fixed sleeps, and each starts its own server so no capacity or state is shared.

`worker/tests/cluster_execute_api.rs` runs the same suite unchanged in a Pod Security restricted
pod (seccomp RuntimeDefault, read-only root filesystem, arbitrary non-root UID) in an
`fs-test-run01-*` namespace, gated by `FS_TEST_CLUSTER` (skipped with an explicit message when
unset), and records one evidence line per case beside the saturation cases.

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
