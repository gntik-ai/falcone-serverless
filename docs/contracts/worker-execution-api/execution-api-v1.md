# ctr-worker-execution-api v1

Contract: ctr-worker-execution-api
Version: 1

This is the Wasm-worker execution API of RUN-01 (FR-RUN-01, FR-RUN-02), the early provider of
`ctr-worker-execution-api` under ADR 0010 D1. It defines the messages and parsers (`worker/src/api`)
and, since RUN-01.t3, the HTTP service that serves them (`worker/src/server`, `falcone-serverless-worker
serve CONFIG`; see [Worker service](#worker-service)). The service reads no credential store: the
execution credential arrives in the request and stays in the host binding layer. The dispatcher
adopts the API in a later issue. The [OCI profile](oci-profile.md) is an independent profile of the
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
A retried attempt of the same activation gets the same status, reason and class: a retry never
changes the class of a code. `busy` is the only `retryable` code and `execution_uncertain` the only
`uncertain` one; every other code is `non_retryable`. The machine-readable table is
[failure-codes.v1.json](failure-codes.v1.json).

| `code` | HTTP status | `detail.reason` | Class | Origin |
| --- | --- | --- | --- | --- |
| `GW_OCI_NOT_AUTHORIZED` | 403 | `oci_not_authorized` | non_retryable | ctr-functions-api v1 |
| `GW_BASE_DISABLED` | 409 | `base_disabled` | non_retryable | ctr-functions-api v1 |
| `GW_BASE_MISMATCH` | 409 | `base_mismatch` | non_retryable | ctr-functions-api v1 |
| `runtime_mode_not_supported` | 409 | `runtime_mode_not_wasm` | non_retryable | worker only |
| `invalid_request` | 400 | `missing_path`, `non_json_body`, `malformed_message`, `malformed_http`, `budget_above_worker_ceiling`, `engine_version_mismatch` or `duplicate_attempt` | non_retryable | worker only |
| `request_too_large` | 413 | `body_too_large`, `headers_too_large` or `request_body_too_large` | non_retryable | worker only |
| `undeclared_import` | 422 | `import_not_declared` or `import_not_allowed` | non_retryable | worker only |
| `busy` | 503 | `at_capacity` | retryable | worker only |
| `execution_timeout` | 504 | `deadline_exceeded` | non_retryable | worker only |
| `execution_failed` | 500 | `memory_budget_exceeded`, `fuel_exhausted`, `host_call_budget_exceeded`, `buffer_limit_exceeded`, `handler_failed`, `handler_trapped`, `invalid_handler_output`, `instantiation_failed`, `artifact_unavailable`, `artifact_rejected` or `cancelled` | non_retryable | worker only |
| `execution_uncertain` | 502 | `host_call_outcome_unknown` | uncertain | worker only |
| `not_found` | 404 | `unknown_path` or `unknown_execution` | non_retryable | worker only |
| `method_not_allowed` | 405 | `method_not_allowed` | non_retryable | worker only |

The three `GW_` codes are referenced, not defined: their names, statuses and reasons are those of
the pinned error fixtures (below). ctr-functions-api v1 defines no other base error code. The
worker-only codes are the other ten: the non-wasm refusal, the two refusals the operator allowed
for the legacy inputs below, and the seven codes of the execution service. Adding `GW_`
equivalents to the functions API is a follow-up. Every message is fixed text: a request this
worker refuses carries no request value in its message, and no refusal names an import, a path or
an identifier.

A failure that is a limit of the revision is an `execution_failed` with a distinct reason, never
a new top-level code (see [Budgets](#budgets)). `busy`, `not_found` and `method_not_allowed` are
sent bare (the failure envelope as the HTTP body) because no activation is known when they are
answered; the result schema also admits them inside a result, with the class of their code.

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

## Worker service

`falcone-serverless-worker serve CONFIG` serves this API from two pod-local listeners
([worker-runtime-config.md](../worker-runtime-config.md)). Ports are configuration, not contract
text; the defaults are shown.

| Listener | Port | Serves | Everything else |
| --- | --- | --- | --- |
| execute | `listen_port` (8080) | `POST /v1/execute` | any other path: 404 `not_found` (`unknown_path`); another method on the path: 405 |
| control | `control_port` (8082) | `POST /v1/cancel`, `POST /v1/update` | any other path, including execute: 404; another method: 405 |

Both listeners bind every address of the pod. The dispatcher reaches the execute listener through
the pool's Service on `listen_port`, and the **control listener at the pod address,
`<pod IP>:<control_port>`, directly**: the control port is never put on a Service. Neither
listener authenticates its caller yet. How the dispatcher authenticates to the worker is not
defined by a frame contract for the Wasm path and is recorded with INV-01; until then the control
port must be reachable only by the dispatcher through network policy (a finding for
gntik-ai/falcone-charts: the chart must publish the execute port only on the Service and
restrict the control port). The execution credential is the request's `credential` claims; it is
not an authentication of the dispatcher to the worker.

### Wire format

HTTP/1.1, one request per connection, always `Connection: close`. `Content-Length` is required;
chunked bodies are refused as `malformed_http`. Bodies are JSON and responses are
`application/json`.

- **Execute.** The body is the execute message. A success answers HTTP 200 with the result. A
  failure answers the HTTP status of its code, with the result (activation attributed, identity
  filled) when the request was read far enough to know the activation, and with the bare failure
  envelope otherwise (a malformed message, `busy`, `not_found`, `method_not_allowed`, an unknown
  runtime mode, a refusal before the body is read).
- **Cancel.** The body is a cancel message with `cancel.state` `requested`. The worker answers 200
  with the same message in state `acknowledged` or `uncertain`. A cancel for an attempt that is not
  running (never admitted, or already finished) answers 404 `not_found` (`unknown_execution`).
- **Update.** The body is an update message for a running execution; the worker validates it and
  answers 204. It forwards nothing yet. A message for an attempt that is not running answers 404.

The guest returns bytes. A JSON object carrying `statusCode` is the legacy handler result and must
satisfy its rules; any other JSON value is the action's result and becomes the `body` of a 200;
output that is not JSON is `execution_failed` (`invalid_handler_output`).

### Limits and deadlines

Every bound is enforced on the socket and on both listeners, never on a buffer. After any breach
the connection is released at once and later requests are unaffected.

| Bound | Value | On breach |
| --- | --- | --- |
| request line and headers (`MAX_HEADERS`) | 16 KiB | 413 `request_too_large` (`headers_too_large`); nothing more is read |
| request body (`MAX_REQUEST`), execute | 8 MiB, by `Content-Length` | 413 `request_body_too_large`; the body is not read |
| request body, control | 16 KiB, by `Content-Length` | 413 `request_body_too_large` |
| action body (`__ow_body`) | 1 MiB | 413 `request_too_large` (`body_too_large`), result envelope; no guest code runs |
| headers, absolute deadline from accept | 5 s | the connection is closed without an answer (`connection_closed`, reason `header_deadline`) |
| body, inactivity (`IO_TIMEOUT`) | 5 s without a byte | closed (`body_inactivity`) |
| body, absolute deadline | 30 s | closed (`body_deadline`) |
| response, one absolute deadline across every write | 5 s | closed at once, thread and slot released (`response_deadline_exceeded`) |

A reader that takes a response one byte at a time is cut off at the response deadline: the
deadline is not renewed by each write.

### Capacity

- **Execute listener.** At most `concurrency` executions run, and at most 8 more connections are
  in admission (reading, or writing a response). An execute beyond that is answered at once with
  `503 busy` (`at_capacity`, retryable) and its body is not read. A request refused before it
  runs (for example a non-wasm mode) takes no running slot.
- **Control listener.** A fixed 16 connections, with its own header, body and response deadlines.
  Execute traffic cannot consume them: it is a different socket with a different counter, so a
  cancel is accepted with every execute slot held and the execute listener full of stalled
  connections. A control connection beyond 16 is answered `503 busy`. Stalling the control listener
  does not affect running executions, and it recovers when its deadlines pass.

### Order of checks

1. HTTP: head, path and method, capacity (above).
2. Parse. The runtime mode is checked first: a mode other than `wasm`, or an image reference
   (`runtime_mode` `oci`), is refused with 409 `runtime_mode_not_supported` before anything else,
   before a slot is taken, and creates no instance. Then malformed messages (400), the credential
   binding, and the legacy inputs.
3. Engine: `projection.engine_version` must equal the Wasmtime this worker embeds (`49.0.5`), or
   400 `engine_version_mismatch`. The identity of a result therefore always names the engine that ran.
4. Budgets: a `memory_bytes` above the worker's `max_memory_bytes` is 400 `invalid_request`
   (`budget_above_worker_ceiling`), before the artifact is read, compiled or instantiated. A
   request whose `deadline` has passed is 504 without running.
5. Artifact: read from the worker's trusted store, `<cache_path>/artifacts/<sha256 hex>.wasm`, and
   its SHA-256 must equal `projection.artifact_digest`; otherwise 500 `artifact_unavailable`. How
   artifacts reach the store (storage fetch) is the dispatcher and storage work, not this API.
6. Declared imports, before compilation: every import of the component must be both allowed by
   the worker (`ALLOWED_IMPORTS`) and listed in `projection.imports`; otherwise 422
   `undeclared_import` (`import_not_allowed`, or `import_not_declared` for an allowed interface the
   revision did not declare). `imports: []` means none. No guest code runs.
7. Trusted compilation, instantiation, execution. Nothing tenant-supplied is deserialized.

### Budgets

The revision's `budgets` are limits, not observations
([revision-runtime-manifest.md](../revision-runtime-manifest.md)). Each is enforced, and a budget the
worker cannot honour is refused, never replaced by a default.

| Budget | Enforcement | Outcome |
| --- | --- | --- |
| `memory_bytes` | guest memory limit = the least of `budgets.memory_bytes`, the worker's `max_memory_bytes` and the 256 MiB compatibility ceiling; instantiation and `memory.grow` beyond it fail | 500 `execution_failed` (`memory_budget_exceeded`) |
| `fuel` | the store's fuel is `budgets.fuel`; the epoch deadline still applies independently | `execution_failed` (`fuel_exhausted`) |
| `max_host_calls` | every guest-to-host call is counted on the store; call `N + 1` is refused before it reaches the host and ends the execution; calls already made are not rolled back | `execution_failed` (`host_call_budget_exceeded`) |
| `timeout_ms` | see below | 504 `execution_timeout` |

All three budget failures are `retryable: false`, return no partial result, and carry the
redacted logs of the calls already made. Fuel accounting changes guest timing: a revision's `fuel`
must cover its instantiation and its run.

### Timeout selection

The effective guest timeout is the least of the time left until the request's `deadline`, the
projection's `budgets.timeout_ms` and the worker's `max_wall_clock_ms`. Compilation counts against
it; a host call never outlives it. On expiry the answer is 504 `execution_timeout` with the
activation attributed and the identity filled, `retryable: false`, and the slot is free again. The
guest is interrupted within one epoch tick (5 ms) of the limit plus scheduling; the qualification
accepts the limit plus 3 s and not less than the limit minus 200 ms.

### Cancel

A cancel sets the execution's cancellation flag, which stops compilation, interrupts the guest
(independently of fuel and epochs) and every host call, and then waits at most `cancel_reserve_ms`
(5 s by default) for the execution to end. It answers `acknowledged` when the execution stopped and
no external effect is in doubt. It answers `uncertain` when the execution did not stop within that
deadline, or when it stopped with a mutating host call in flight whose effect may or may not have
happened; no state claims that external side effects were reversed. The cancelled execute answers
500 `execution_failed` (`cancelled`), or `execution_uncertain` (502). A host call that is blocked
cannot be interrupted before RUN-02 adds host-call deadlines, so the outcome is `uncertain` there.

### The credential and logs

The execution credential is held only by the service's host binding layer. It is never placed in
the host context, a WASI environment, a preopened file, guest memory or an export, and the worker
does not read it from its own environment. The guest has no WASI import at all (an import outside
the allowed list is refused), so it has no environment or file to read.

Guest log lines (the `logging` capability) are returned in `logs` and copied to the worker's
structured log as `guest_log` events. Every string of the credential claims is redacted from them:

1. each line is clipped to 8192 characters, and to 1000 lines, **before** redaction;
2. one multi-pattern matcher (Aho-Corasick) is built per execution and each line is scanned once;
   the matches of all secrets are merged, so overlapping, nested and prefix-sharing secrets leave
   no residue, and every secret of any length, down to one character, is replaced by `<redacted>`;
3. the work is bounded by a fixed budget of 16 MiB of work units (credential bytes, scanned bytes
   and matches). Over budget, the logs are dropped and the result carries the single line
   `<redacted>`, never unredacted text. Within budget, 4 MiB of distinct credential strings and
   1000 maximum-length lines redact in under 1 s in a release build, and the slot is released.

The same redaction applies to every failure, including the budget failures. Refusal messages are
fixed text and carry no request value.

### Structured worker log

One JSON object per line on standard error. Fields are identifiers, numbers and fixed labels; the
only request-derived text is the redacted `guest_log` line. The `activation_id` and `attempt_id`
that attribute an event are identifiers of a safe alphabet, not secrets, even though the credential
binds them (guest log lines are redacted of them all the same, which is the fail-closed choice). Events: `listening` (bound addresses),
`execution_admitted`, `guest_started` (just before guest code is entered), `guest_log`,
`execution_finished` (`http_status`, `host_calls`, `elapsed_ms`), `cancel_answered`,
`connection_closed` (`listener`, `reason`) and `response_deadline_exceeded`. The qualification
synchronizes on these events.

### Qualification

`worker/src/qualification/execution_api.rs` holds the black-box cases: they start the real worker,
publish embedded fixture components to its artifact store and talk to it over TCP. They run locally
(`worker/tests/execute_api_local.rs`) and in the restricted fs-test pod (`worker/tests/cluster_execute_api.rs`,
namespace `fs-test-run01-<run-id>`, Pod Security restricted, seccomp `RuntimeDefault`, read-only root
filesystem, arbitrary non-root UID). See [the qualification record](../../qualification/run-01/execution-api.md).

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
