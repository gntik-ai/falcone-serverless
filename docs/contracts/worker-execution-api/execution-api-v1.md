# ctr-worker-execution-api v1

Contract: ctr-worker-execution-api
Version: 1

This is the Wasm-worker execution API of RUN-01 (FR-RUN-01, FR-RUN-02), the early provider of
`ctr-worker-execution-api` under ADR 0010 D1. `worker/src/api` defines the messages and parsers;
`worker/src/server` serves them through the pod-local HTTP transport described below. The
dispatcher adopts this transport in INV-01. The
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

## Pod-local HTTP transport (RUN-01)

Start the worker with `falcone-serverless-worker serve CONFIG`, where CONFIG is the versioned
[RuntimeConfig TOML](../worker-runtime-config.md). Execute uses `listen_port` inside the pod;
cancel/update use a separate control listener on **`listen_port + 2`** (defaults: **8080** and
**8082**). Reserve both ports for the worker; serve requires `listen_port <= 65533` and both
binds to succeed. The offset is fixed and adds no field to the closed RuntimeConfig v1 schema.
The execute listener still accepts cancel/update for compatibility when it has capacity, but
dispatchers **must use the control listener** for cancellation under load. Execute and unknown
paths on the control listener receive HTTP 400 `invalid_request` at the request line, before
reading the rest of the headers or the body; execute is never dispatched there.
The existing version, configuration-check and explicit execute commands retain their behavior.
This is an HTTP/1.1 JSON POST transport: one request per connection, `Content-Length` required,
`Content-Type: application/json`, no transfer encoding, at most 16 KiB of headers and 8 MiB of
encoded request data. Connections close after the response. Header transfer has an absolute
5 second deadline from acceptance; sending a byte does not renew it. Body reads and socket
writes retain their 5 second inactivity allowance. These transport bounds do not replace the
legacy body checks. An encoded request over 8 MiB is a framing error: HTTP 400
`invalid_request`. HTTP 413 `request_too_large` remains the complete legacy refusal
envelope with `detail.reason: body_too_large` for the action body limit below.

| Path | Operation |
| --- | --- |
| `/v1/execute` | Parse/admit an execute message, validate the component and declared imports, compile through the trusted child, then create a fresh instance. A successful response is HTTP 200 with the result envelope above. |
| `/v1/update` | Validate an update for an active activation/attempt and acknowledge its transport with the unchanged update message: HTTP 202 during setup, HTTP 200 after the handler is prepared. An unknown attempt receives HTTP 400. This endpoint makes no durable activation-state claim; Falcone owns persistence and dispatch. |
| `/v1/cancel` | Parse a requested cancel and reply with the same activation/attempt and state acknowledged or uncertain. Unknown/already-finished attempts return uncertain. Other input states are rejected. |

The trusted embedding/intake stages raw components in
`cache_path/components/<component_digest hex>.wasm`; the request selects a file only through its
validated digest. The worker checks the actual digest and component type. It checks every import
against **both** `ALLOWED_IMPORTS` and `projection.imports`, after validation and before opening
the native cache, spawning a compiler or instantiating. Empty declarations admit no imports.
An import rejected by either check receives HTTP 422 `undeclared_import`; a declared allowed
component proceeds. This check runs again for each request, including native-cache hits.
The native cache is process-owned under `cache_path/native`; native code comes only from the
trusted `artifact-compile-child` beside the worker executable, never from a request-supplied path
or tenant precompiled artifact. The engine pin must equal the worker's compiled-in Wasmtime
version. Identity fields come from the projection, with that compiled-in engine version.

At most `concurrency` connections are admitted on the execute listener (zero is invalid in serve
mode), including connections that have not sent a single byte, incomplete headers/bodies, and
running executions. **Four fixed control slots** belong exclusively to the separate control
listener; this reserve is not operator-configurable. The total admitted connection limit across
both listeners is `concurrency + 4`, including when concurrency is 64 or greater. An execute
connection over capacity receives HTTP 503 `busy` immediately at acceptance, without reading
headers/body, compiling or retaining a connection while waiting for an execution slot. Even
incomplete execute request lines cannot occupy the control listener's capacity.
After writing the response, the worker shuts down the socket's write side and discards
queued input with a bounded, nonblocking read before closing; it never waits for an
over-capacity sender to finish sending its request.

There can be at most four simultaneous control connections, including their transfer time and
any cancellation wait up to `cancel_reserve_ms`. A fifth control connection receives HTTP 503
`busy`; dispatchers must bound simultaneous control requests and retry excess cancels. This
reserve guarantees availability against execute saturation, not against control saturation.

Cancellation never waits for the compiler lock or an execution slot. Its processing deadline is
`RuntimeConfig.cancel_reserve_ms` (default **5000 ms**), measured after the cancel is parsed; socket
transfer time is separate. The worker signals the attempt's cancellation flag and waits at most
that interval for completion. A completed interruption without an uncertain host outcome reports
`acknowledged` only for `GuestInterrupted` or `Cancelled` outcomes with cancellation requested.
All other outcomes, including a cancel racing with an already-final success, failure or
resource-limit outcome, report `uncertain` by that deadline. Cancellation interrupts guest
code through the existing epoch mechanism; it never claims external side effects were undone.
Host-call deadlines and full drain/probe/limit policy remain RUN-02 work. Existing host safety
ceilings remain enforced.

Admission refusals use the exact envelopes, statuses and reasons in the tables below. Malformed
messages return HTTP 400 `invalid_request`. Component/compile/handler failures use the existing
worker diagnostic code (for example HTTP 422 `undeclared_import` or `cancelled`) as a transport
error object; they do not invent a new failure envelope code or change the v1 schemas.

The execution credential remains in host-side request state. Only action params and the explicit
invocation identity enter the guest; no credential is placed in store context, WASI environment,
preopens or the guest log binding. Guest log fields retain the host's redaction and returned logs
also redact issuer/audience values of any length and other claim strings of at least eight bytes.
This preserves ordinary words when identity/grant claims are short while still removing the
credential (including its JSON-escaped issuer/audience). Structured worker diagnostics contain fixed events/statuses,
and the serve panic hook never formats panic payloads. No request/error values are logged.

Dispatcher-to-worker authentication and credential verification for platform capabilities remain
undefined on this Wasm path (finding for `gntik-ai/falcone`, INV-01). Platform bindings therefore
fail closed; this service grants no storage, HTTP, event, workflow or secret authority from an
unverified claim. ADR 0006 remains Proposed. Pools stay at zero until restricted RUN-01
qualification, the R3 second review (explicitly checking imports and control capacity), and
operator approval. This change does not authorize deployment.

`worker/tests/execute_api_local.rs` exercises HTTP framing/admission with an in-memory transport,
import preflight, identity construction, credential redaction and capacity at 1 and 65 without
executing guests. `worker/tests/cluster_execute_api.rs` uses the existing namespace harness with
`fs-test-run01-<run-id>` and the `--execution-api` qualification runner. It covers live HTTP,
identity/base pins, all import refusals plus declared/allowed success, forbidden environment/files
imports, a guest memory scan with a positive canary control, guest/worker logs, and blocked-guest
saturation/cancel at 1 and 65. Claims are synthetic fixtures; no real credential is retrieved.
The harness enforces restricted PSS, arbitrary non-root UID, RuntimeDefault seccomp, read-only
root filesystem and owned namespace cleanup on success/failure; shared mode retains its existing
run-label ownership checks. Run with `FS_TEST_CLUSTER=1`, a digest-pinned `FS_TEST_WAS02_IMAGE`
and the existing kubeconfig/namespace-mode settings, using
`cargo test --locked -p falcone-serverless-worker --test cluster_execute_api -- --include-ignored`.
An unset gate visibly skips live qualification.

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
| `invalid_request` | 400 | `missing_path` or `non_json_body` | non_retryable | worker only |
| `request_too_large` | 413 | `body_too_large` | non_retryable | worker only |

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
