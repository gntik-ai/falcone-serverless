# ctr-worker-execution-api v1

Contract: ctr-worker-execution-api
Version: 1

This is the Wasm-worker execution API of RUN-01 (FR-RUN-01, FR-RUN-02), the early provider of
`ctr-worker-execution-api` under ADR 0010 D1. It defines the messages and parsers (`worker/src/api`,
which execute no guest) and the worker's HTTP service that carries them (`worker/src/server`,
[below](#worker-http-service)). The dispatcher adopts it in a later issue. The
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

## Worker HTTP service

`falcone-serverless-worker serve CONFIG ARTIFACTS` serves the API on `listen_port` of the runtime
config, from the components in the `ARTIFACTS` directory (`<64 hex digits>.wasm`, checked against
the projection's `artifact_digest`). The service takes one request per connection and always closes
it after the reply. It does not authenticate the dispatcher: that is INV-01, and pools stay at zero
replicas until the RUN-01 restricted qualification passes.

| Operation | Request | Reply |
| --- | --- | --- |
| `POST /v1/execute` | an `execute` message | `200` with a `success` result, or the failure status of the result's code |
| `POST /v1/cancel` | a `cancel` message in state `requested` | `200` with the same message in state `acknowledged` or `uncertain` |
| `POST /v1/update` | an `update` message of kind `heartbeat` | `200` with the attempt's latest `progress` or `checkpoint` update, or a `heartbeat`; `404` when the attempt is not running |

A message that does not match its schema is a `400` without a body. A result always repeats its
`activation_id`, `attempt_id` and `attempt_number` and carries the identity block of its
projection, with `wasmtime_version` the pinned engine; the worker refuses a projection whose
`engine_version` is not the version it embeds (`execution_failed`, reason `engine_mismatch`).

### Order of checks of an execute request

1. The admission of the contract, in its order: a `runtime_mode` other than `wasm` is refused
   (`runtime_mode_not_supported`) before anything else; then grants, credential binding and the
   legacy inputs (`missing_path`, `body_too_large`, `non_json_body`).
2. Capacity (below; an execute request over capacity is refused only by item 1 or `busy`), the pinned engine, the deadline, the artifact and its digest.
3. The declared-import check, before any compile or instantiation: every import of the component
   must be in the projection's `imports` **and** in the host's allow-list; otherwise `422
   undeclared_import`, with no compile, no instance and no guest code. `imports: []` means none.
   A declared import that is not allow-listed (for example ambient `wasi:` interfaces) is refused
   the same way: a guest has no environment, preopened file or clock beyond the host interfaces.
4. Compilation through the trusted compile child only, then instantiation and the call.

The handler's output must be JSON. An object with a `statusCode` is the legacy result and must be
exactly `{statusCode, headers?, body?}`; any other JSON value is the body of a `200`. An output
that is neither is `execution_failed` with reason `invalid_handler_result`. Failures of a running
guest are `execution_failed` (HTTP 500) with the reason of what happened: `handler_failed`,
`handler_trapped`, `invalid_input`, `deadline_exceeded`, `cancelled`,
`guest_memory_limit_exceeded`, `buffer_limit_exceeded`, `uncertain` (a detached host call may still
commit). Only `timeout_ms` of the projection's budgets is applied here; the other budgets are
enforced by RUN-02.

### Capacity and cancel

At most `concurrency` execute requests run at once. A fixed reserve of `4` control slots serves
cancel and update and is never used by execute requests, so the connection limit is `concurrency +
4`. A connection over the limit is closed before a byte is read. A control request when the
reserve is in use gets `503` with the bare `busy` envelope (`retryable`, with `Retry-After: 1`) as
soon as its head is read. An execute request over capacity holds no execute slot: its body is read
within the same ceilings and deadlines so that the contract's admission can come first, and a
`runtime_mode` other than `wasm` is answered `409 runtime_mode_not_supported` (non-retryable)
exactly as when the worker is idle; any other execute request is answered `503 busy` as soon as
that check has passed, without parsing further, compiling or running anything. The `busy` reply
has no activation to attribute, so it is the failure envelope itself rather than a result.

Cancel interrupts the guest and waits for it to stop, up to `cancel_reserve_ms` (default 5 s). It
answers `acknowledged` once the guest has stopped, and `uncertain` when it has not stopped within
that deadline or a detached host call may still commit. An attempt that is not running is
acknowledged: there is nothing left to stop. Cancel cannot interrupt a blocked host call before
RUN-02 adds host-call deadlines; that is the `uncertain` case. The cancelled execute itself ends
with `execution_failed`, reason `cancelled`.

### HTTP ceilings and deadlines

| Limit | Value | Over the limit |
| --- | --- | --- |
| Request line and headers (`MAX_HEADERS`) | 16 KiB | `431`, connection closed |
| Execute request, head plus body (`MAX_REQUEST`) | 8 MiB | `413` with the `request_too_large` envelope, before the body is read |
| Cancel or update body | 16 KiB | `413`, as above |
| Action body (`__ow_body`) | 1 MiB | the contract's `413 body_too_large` result |
| Header section, absolute (`HEADER_DEADLINE`) | 5 s | connection closed, however slowly the bytes arrive |
| Any read without progress (`IO_TIMEOUT`) | 5 s | connection closed, including a silent connection and a stalled body |
| Body, absolute | 30 s | connection closed |

A request without `Content-Length` is `411`; a chunked body is `501`; a repeated `Content-Length`
or a malformed request is `400`; an unknown path is `404` and a method other than `POST` is `405`.
None of these replies has a body, except the `413` envelope.

### Logs and the credential

The execution credential stays in the host binding layer. It is never passed to Wasmtime or a
WASI context, to guest memory, to a preopened file, to a log line or to an error string. The
strings of the credential (claims and grant scopes) are redacted from the guest's logs:

- each log line is cut to 8192 characters before it is scanned, and at most 1000 lines are kept;
- one multi-pattern matcher is built per execution and run as a single overlapping pass over each
  line, so the work is linear in the line length plus the secret set; the bytes read and every
  occurrence reported are charged to one fixed budget (16 MiB), as they happen, so a secret set
  that matches very often (for example prefixes of one another over a repetitive line) is cut off
  by the budget and never scanned to the end;
- overlapping and prefix-sharing secrets are redacted whole;
- every non-empty string of the credential is a secret, however short; more than 4096 distinct
  secrets, a secret over 4096 bytes, more than 256 KiB of secrets, or work over the budget fail
  closed: the logs are dropped and a single `<redacted>` line is returned.

The documented time bound: the largest secret set plus 1000 lines of 8192 characters is redacted in
under 1 s in a release build (`worker/tests/execute_api_local.rs` asserts it; the measured time is
about 0.1 s), and the execute slot is released afterwards. Over-budget inputs return the marker
line in the same time.

Failure messages are fixed text per code and carry no request value.

## Failure codes and classes

A failure `result` is `{status, code, message, detail: {reason}, retryable}`, shaped as the
ErrorResponse of ctr-functions-api v1 without the request-correlation fields. The class is one of
`retryable`, `non_retryable` and `uncertain`; `retryable` is true exactly for class `retryable`.
Every code is `non_retryable` except `busy`, including on every retry: a retried attempt of the
same activation gets the same status, reason and class. The `busy` class is `retryable` and
`retryable` is true. Later revisions only add codes; an existing code keeps its status, reason and
class. The machine-readable table is [failure-codes.v1.json](failure-codes.v1.json).

| `code` | HTTP status | `detail.reason` | Class | Origin |
| --- | --- | --- | --- | --- |
| `GW_OCI_NOT_AUTHORIZED` | 403 | `oci_not_authorized` | non_retryable | ctr-functions-api v1 |
| `GW_BASE_DISABLED` | 409 | `base_disabled` | non_retryable | ctr-functions-api v1 |
| `GW_BASE_MISMATCH` | 409 | `base_mismatch` | non_retryable | ctr-functions-api v1 |
| `runtime_mode_not_supported` | 409 | `runtime_mode_not_wasm` | non_retryable | worker only |
| `invalid_request` | 400 | `missing_path` or `non_json_body` | non_retryable | worker only |
| `request_too_large` | 413 | `body_too_large` | non_retryable | worker only |
| `undeclared_import` | 422 | `undeclared_import` | non_retryable | worker only |
| `busy` | 503 | `busy` | retryable | worker only |
| `execution_failed` | 500 | `handler_failed`, `handler_trapped`, `invalid_input`, `invalid_handler_result`, `deadline_exceeded`, `cancelled`, `guest_memory_limit_exceeded`, `buffer_limit_exceeded`, `artifact_unavailable`, `artifact_rejected`, `artifact_digest_mismatch`, `engine_mismatch`, `uncertain`, `worker_error` | non_retryable | worker only |

The three `GW_` codes are referenced, not defined: their names, statuses and reasons are those of
the pinned error fixtures (below). ctr-functions-api v1 defines no other base error code. The
worker-only codes are the last six: the non-wasm refusal, the two refusals the operator allowed
for the legacy inputs below, and the three codes of the worker service (RUN-01.t3), added without
changing any earlier code. Adding `GW_` equivalents to the functions API is a follow-up. A request this worker refuses carries no request value in its message.

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
  the worker-only codes are exactly the six above;
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
