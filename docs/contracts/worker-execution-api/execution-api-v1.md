# ctr-worker-execution-api v1

Contract: ctr-worker-execution-api
Version: 1

This is the Wasm-worker execution API of RUN-01 (FR-RUN-01, FR-RUN-02), the early provider of
`ctr-worker-execution-api` under ADR 0010 D1. It defines the messages, their parsers, and the
HTTP service of the worker (`worker/src/server`, started by `falcone-serverless-worker serve`)
that carries execute, update and cancel; see [Listeners](#listeners). How the dispatcher
authenticates to the service is not defined here: INV-01 (gntik-ai/falcone) records it. The
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
| `invalid_request` | 400 | `missing_path`, `non_json_body`, `malformed_request`, `budget_above_worker_ceiling`, `engine_version_mismatch` or `artifact_digest_mismatch` | non_retryable | worker only |
| `request_too_large` | 413 | `body_too_large` | non_retryable | worker only |
| `busy` | 503 | `no_capacity` | non_retryable | worker only |
| `undeclared_import` | 422 | `undeclared_import` | non_retryable | worker only |
| `timeout` | 504 | `deadline_exceeded` | non_retryable | worker only |
| `cancelled` | 499 | `cancel_requested` | non_retryable | worker only |
| `execution_failed` | 500 | `memory_budget_exceeded`, `fuel_exhausted`, `host_call_budget_exceeded`, `buffer_limit_exceeded`, `handler_failed`, `handler_trapped`, `instantiation_failed`, `invalid_handler_result`, `artifact_rejected`, `artifact_unavailable`, `compile_failed`, `setup_failed` or `uncertain_side_effects` | non_retryable | worker only |

The three `GW_` codes are referenced, not defined: their names, statuses and reasons are those of
the pinned error fixtures (below). ctr-functions-api v1 defines no other base error code. The
worker-only codes are the other eight: the non-wasm refusal, the two refusals the operator
allowed for the legacy inputs below, and the five outcomes of serving an execution (`busy`,
`undeclared_import`, `timeout`, `cancelled`, `execution_failed`). Every further failure is a
`detail.reason` of one of them, never a new code. Adding `GW_` equivalents to the functions API
is a follow-up. A request this worker refuses carries no request value in its message.

`cancelled` has status 499 because no standard status says that the caller withdrew the request.
`busy` is `non_retryable` as a class like every v1 code: nothing ran, and the dispatcher decides
what to do with a refusal that depends on capacity, not on the request. A later revision may give
it another class.

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

## Listeners

The worker serves the API on two pod-local listeners, configured in the
[runtime configuration](../worker-runtime-config.md) (`listen_address`, `listen_port`,
`control_port`). They are separate sockets with separate capacity: no traffic on one can take
capacity from the other.

| | Execute listener | Control listener |
| --- | --- | --- |
| Address | `listen_address:listen_port` (default 8080) | `listen_address:control_port` (default 8082) |
| Serves | `POST /v1/execute` only | `POST /v1/cancel` and `POST /v1/update` only |
| Anything else | `404` (including the other listener's operations) | `404` (including `/v1/execute`) |
| Connections | `concurrency` running executions plus 8 in admission | 16 |
| Header limit and deadline | 16 KiB, 5 s absolute from accept | 4 KiB, 2 s absolute from accept |
| Body limit and deadline | 8 MiB, 10 s absolute from the end of the headers | 64 KiB, 2 s absolute from the end of the headers |
| Response deadline | 5 s absolute from the first response byte | 2 s absolute from the first response byte |

The control transport is a second TCP port, not a Unix socket: the dispatcher runs in another
pod, so it reaches control at the worker pod's address on `control_port` (a Service or the pod
IP; exposing it is a chart concern). How the dispatcher authenticates to either listener is
undecided and recorded with INV-01; until then the listeners carry no authentication and must be
reachable only through the NetworkPolicy of the pool.

Every deadline is an absolute instant: each read or write gets only the time that is left, so a
peer that sends or reads one byte at a time cannot extend it. A connection that has no complete
request line and headers by the header deadline, whose body does not complete by the body
deadline, or that does not take the whole response by the response deadline, is closed. There is
one request per connection (`Connection: close`), no keep-alive, no chunked request bodies and no
upgrade.

An execute request that exceeds a size limit is refused without running a guest: headers over
16 KiB get `431` (or the connection is closed), a `Content-Length` over 8 MiB gets `413` with the
`request_too_large` envelope before any body is read, and an `__ow_body` over 1048576 bytes gets
`413` with the attributed result of the legacy-input table above. Control messages over their
limit get `413` the same way. A response that is not an execution result (the `404`, `411`, `431`
and malformed-HTTP answers) is `{"error": "<name>"}`; a refusal that has no activation to
attribute (`busy`, malformed, over-large) is the bare failure envelope of the table above.

### Admission

The execute listener answers a connection beyond its connection limit with `503 busy` at once and
reads nothing. Otherwise, after the headers:

1. With every execution slot busy the body is not read, with one exception. A body that arrived
   with the headers and is complete is parsed, and a request whose `runtime_mode` is not `wasm`
   still gets `409 runtime_mode_not_supported` first. Anything else gets `503 busy`.
2. With a free slot the body is read, parsed and refused or admitted as above. A request that
   loses the race for the slot, or repeats the `activation_id` and `attempt_id` of a running
   execution, gets `503 busy`.

An execution slot is held from admission to the end of the execution, never while the response is
written: a client that reads slowly holds a connection until the response deadline, not a slot.

### One absolute deadline

`until` is computed once, at admission:

`until = admission + min(request deadline - now, projection budgets.timeout_ms, max_wall_clock_ms)`

The request limit is the execute message's `deadline` (an absolute UTC time); a deadline that has
passed leaves no time, and no stage starts. The same `until` bounds, in order:

1. fetching and validating the artifact, on a helper the execution can leave;
2. waiting for and running the supervised compile child (its wall clock is `until`);
3. `Platform::bind`, on a helper the execution can leave;
4. instantiation (the store's epoch deadline is derived from `until`), and a check that `until`
   has not passed after instantiation and before the handler is invoked;
5. every host call: its timeout is `min(host-call ceiling, until - now)`, and a call that starts
   at or after `until` is refused without reaching the host. The deadline is checked before the
   call is counted, so a late call ends the execution as `timeout` even when it would also have
   exceeded `max_host_calls`.

A stage that crosses `until`, or sees the cancellation, ends the execution as `504 timeout` or
`499 cancelled` before the guest starts, and the execution slot is released within the
termination budget of 500 ms: the artifact is validated on a helper thread that the execution
leaves (the helper finishes on its own, and the number of live helpers is capped), never in the
execution's own thread. Each artifact is validated exactly once per execution. A blocked
`Platform::bind` can neither hold the slot past `until` nor ignore the cancellation.

An execution that reaches `until` answers within the termination budget of 500 ms after it; the
qualification measures this against the 500 ms of cancellation and timeout during validation, and
against a tolerance of one second for the timeout-selection cases, which include scheduling.

A host call that blocks inside the host ends at `until`. Cancelling a guest that is blocked in a
host call cannot interrupt the call itself before RUN-02: the guest gets the timeout or the
cancellation when the call returns or at `until`, and the cancel answer is `uncertain`.

### Imports

Every import of the component must be declared in the projection's `imports` and be one the worker
provides (`ALLOWED_IMPORTS`); an empty `imports` means none. Any other import, whether the
projection omits it or the worker does not provide it, ends the request as `422 undeclared_import`
before anything is compiled or instantiated.

### Budgets

The projection's `budgets` are limits, enforced and never replaced by a default.

| Budget | Enforcement | Failure |
| --- | --- | --- |
| `memory_bytes` | guest memory is the least of the budget, `max_memory_bytes` and the compatibility ceiling (256 MiB); instantiation or `memory.grow` past it fails the execution | `execution_failed` / `memory_budget_exceeded` |
| `fuel` | the store's fuel; running out ends the execution | `execution_failed` / `fuel_exhausted` |
| `max_host_calls` | every guest-to-host entry counts, including storage open and drop callbacks and calls the grants deny; the call after the last is refused before it reaches the host and ends the execution | `execution_failed` / `host_call_budget_exceeded` |
| `timeout_ms` | part of `until` above | `timeout` / `deadline_exceeded` |

A `memory_bytes` above the worker's `max_memory_bytes` (or above the compatibility ceiling) is
refused before anything is compiled, as `invalid_request` / `budget_above_worker_ceiling`. A
projection whose `engine_version` is not the worker's pinned Wasmtime is refused as
`invalid_request` / `engine_version_mismatch`, because the result reports that version as the
runtime identity. An artifact whose digest is not the projection's `artifact_digest` is refused as
`invalid_request` / `artifact_digest_mismatch`.

### Outcomes

| Outcome | Result |
| --- | --- |
| The guest returned a legacy result | `200`, `status` `success`, `result` the handler result, `identity` of the projection |
| The deadline passed (including inside a host call, or before the guest started) | `504 timeout` |
| The cancellation was observed | `499 cancelled` |
| A mutating host call was in flight when the execution ended | `500 execution_failed` / `uncertain_side_effects` |
| Memory, fuel or host-call budget exceeded | `500 execution_failed` with that reason |
| Handler error, trap, failed instantiation, unusable result, rejected or unavailable artifact, failed compile or bind | `500 execution_failed` with the matching reason |

Every failure that ran, or that could have, carries `activation_id`, `attempt_id`,
`attempt_number`, the `identity` block of the projection and the logs.

### Cancel and update

`POST /v1/cancel` takes a `cancel` message in state `requested`. The worker sets the cancellation
of the running execution identified by `activation_id` and `attempt_id` and waits at most 1 s
(`CANCEL_WAIT`) for it to end. The answer is `200` with a `cancel` message in state
`acknowledged` (the execution ended, with no mutating host call left in flight) or `uncertain`
(it did not end within the wait, or ended with a mutating host call that may still complete
because a host call cannot be interrupted inside the host before RUN-02, or it is not running: it
may have ended on its own or never started). No answer claims that external side effects were
reversed.
A message in another state, or a malformed one, is `400`.

`POST /v1/update` takes an `update` message. For a running execution the worker records it (the
highest `sequence` wins) and answers `204`; otherwise `404`. The worker does not forward updates.

### Logs and the credential

The execution credential is held only by the host binding layer (`Platform::bind`). It is not part
of the guest's input, environment, files or memory: the guest gets the `params` object, the
invocation identity and the capabilities its grants name. Every string of the credential claims is
redacted from the logs of a result:

1. each log line is cut to 8192 characters, and at most 1000 lines are kept, before anything else;
2. one multi-pattern matcher is built per execution, every occurrence of every string is found,
   overlapping and prefix-sharing ones included, and each covered span becomes `<redacted>`;
3. the output is cut to 8192 characters again, because a replacement can be longer than what it
   replaced;
4. the work is bounded: at most 100000 distinct strings, 1 MiB of string bytes and 8000000
   matches per execution. Past any of them the logs are dropped and the result carries the one
   line `<redacted>`, never unredacted text.

The worker's own messages carry no request value: errors name a fixed code or reason.

### Platform

`Platform` is the host binding layer's boundary: it supplies the artifact the plan's
`artifact_digest` names and binds the execution's capabilities. The production worker's
`LocalPlatform` reads artifacts from the read-only directory `artifact_path`, as
`<sha256 hex>.wasm`, and grants nothing remote: storage, HTTP, events and workflows are denied,
logging and progress are accepted and dropped. The remote capability bindings follow the
dispatcher and Falcone contracts (INV-01); until then a pool stays at zero replicas, and the kill
switch is zero replicas or rolling back the image digest.

### Not in this contract

Drain, probes, and limit enforcement beyond the per-execution budgets and the deadline above are
RUN-02. ADR 0006 (the credential format) and ADR 0010 (the OCI execution path) are Proposed and
not claimed accepted here.

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

Execution behaviour is covered by `worker/tests/execute_api_local.rs` (real sockets, in
process) and `worker/tests/cluster_execute_api.rs` (the restricted-pod qualification; see
[the qualification record](../../qualification/execute-api/README.md)).

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
