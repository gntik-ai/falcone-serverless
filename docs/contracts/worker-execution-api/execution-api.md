# ctr-worker-execution-api: execution API v1

Contract: ctr-worker-execution-api
Version: 1 (`contract_version` is the string `1`)

This is RUN-01.t1 / FR-RUN-01 and FR-RUN-02 from the [design](../../falcone-serverless-design.md).
It defines the messages between the dispatcher and an execution worker: execute an attempt,
report updates, cancel, and return a result. It defines the contract only. It adds no OCI
execution and runs no guest code. The OCI transport rules live in the separate
[OCI profile](oci-profile.md).

Falcone remains authoritative for revision and activation state (ADR 0009, still Proposed) and
the execution credential follows ctr-execution-credential v1 (ADR 0006, still Proposed). Neither
is claimed as accepted here; this contract follows the operator decisions recorded in issue #124.

The JSON Schemas are the machine-readable form: [common](common.v1.schema.json),
[execute request](execute-request.v1.schema.json), [update](execution-update.v1.schema.json),
[cancel](cancel.v1.schema.json) and [result](execution-result.v1.schema.json). Members are
`snake_case`, except the grant members `secretRef` and `guestGrantId`, which keep the revision
runtime manifest spelling. Digests are `sha256:` plus 64 lowercase hex digits.

## Execute request

An attempt is `{contract_version, activation_id, attempt_id, attempt_number, deadline,
credential, projection, params}`. The object is closed.

| Member | Meaning |
| --- | --- |
| `activation_id`, `attempt_id` | Non-empty strings of at most 256 characters. They identify the activation and this attempt. |
| `attempt_number` | Integer of at least 1. A retry is a new attempt with a higher number. |
| `deadline` | UTC RFC 3339 timestamp ending in `Z`, a real calendar date. |
| `credential` | The ctr-execution-credential v1 claims object, not an opaque string. The worker binds the attempt to it and never forwards it to a guest. |
| `projection` | The pinned revision, below. |
| `params` | The legacy `main(params)` object. |

The credential's `activation`, `attempt` and `revision` must equal `activation_id`, `attempt_id`
and `projection.revision`. The schema cannot say that, so only the parser enforces it.

### Revision projection

The projection is open to additive members: a v1 consumer ignores a member it does not know.
Members beyond those below are always optional, so later revisions stay additive.

| Member | Required | Meaning |
| --- | --- | --- |
| `revision` | yes | The function revision. |
| `runtime_mode` | yes | `wasm` or `oci`. Falcone defaults a missing mode to `wasm` upstream; the worker never does. |
| `base` | no | Absent when the revision pins no base; see below. Never `null`. |
| `artifact_digest` | yes | Digest of the raw tenant artifact. |
| `language_profile` | yes | Closed `{name, version}` pin. |
| `component_digest` | no | Digest of the language profile's trusted component, when it has one. |
| `engine_version` | yes | The Wasmtime build the revision requires. |
| `wasi_version`, `interface_version` | yes | The pinned WASI and `falcone:host` interface versions. |
| `declared_imports` | yes | Import names the artifact declares. |
| `budgets` | yes | Closed `{fuel, memory_bytes, timeout_ms, max_host_calls}`, positive integers. |
| `granted_capabilities` | yes | The revision's grants, validated against the revision runtime manifest grant definitions at `@0.2.0`. |

### Runtime mode

`runtime_mode` is required. A Wasm worker executes `wasm` and refuses anything else: a well-formed
request for `oci` is answered with the worker-only refusal `runtime_mode_unsupported`
(non-retryable, never executed, no fallback). A value outside `wasm|oci` is malformed and is not
executed either.

### Base

A base is a tagged union on `kind`, or absent:

- `{kind: "catalog", id, version, digest}`: a catalog base pinned by a concrete `id@version` and
  its digest. `id` matches `^[a-z0-9][a-z0-9._-]{0,127}$` and `version` matches
  `^[0-9][0-9A-Za-z.+_-]{0,63}$`.
- `{kind: "byo", image_digest, signer_identity, attestation_ref}`: a bring-your-own image.

An unresolved default alias is not a base. Falcone resolves the per-mode default alias at deploy
and pins the result, so `{"kind": "alias"}`, a version such as `default` or `latest`, a catalog
base without a digest, and an explicit `null` are all malformed.

## Updates and cancel

An update is `{contract_version, activation_id, attempt_id, sequence, kind, ...}` with `sequence`
of at least 1 and `kind` one of `heartbeat` (no other member), `progress` (optional `percent` 0 to
100 and `message`, 1 to 256 characters) or `checkpoint` (required `checkpoint_ref` and `digest`).
A cancel message is `{contract_version, activation_id, attempt_id, state}` with `state`
`requested` (dispatcher to worker), `acknowledged` (the worker stopped the attempt) or `uncertain`
(the worker cannot confirm the attempt stopped).

## Result

A result is `{contract_version, activation_id, attempt_id, status, result | error, logs,
identity}`. It carries the legacy `status`, `result`, `logs` envelope:

- `status: "success"`: `result` is the function's legacy envelope `{statusCode, headers?, body?}`
  and `error` is absent.
- `status: "failure"`: `error` is `{status, code, message, detail: {reason}, retryable, class}` and
  `result` is absent.
- `logs` is an array of at most 1000 strings of at most 4096 characters.

A member the contract says is absent must be absent. An explicit `null` is not absence: it is
rejected for `result`, `error`, `headers`, `body`, `identity.base_digest` and every other optional
member of every message.

### Identity block

Every result carries `identity`: `function_revision`, `runtime_mode`, `artifact_digest`,
`base_digest`, `wasmtime_version`, `language_profile` and `interface_version`. It repeats the
request's projection. `base_digest` is present exactly when the projection pins a base (the
catalog `digest` or the BYO `image_digest`) and absent otherwise. `parse_result` rejects a pinned
base without `base_digest`, a `base_digest` without a pinned base, `null` in either case, and a
digest that is not the pinned one. The schema alone cannot check this, because it does not see the
request.

## Failure codes

The `GW_` codes are the ctr-functions-api v1 codes, referenced by name and meaning and not
redefined; their status, reason and message are checked against pinned copies of the published
error fixtures. The three worker-only codes are the only codes defined here. Every v1 code is
non-retryable on every path, including a retry attempt (`attempt_number` above 1): the same revision
fails the same way again. The classes `retryable` and `uncertain` are reserved for codes that later
revisions add, and a v1 parser rejects a result that claims them for a v1 code.

| Code | HTTP status | Reason | Class | Origin |
| --- | --- | --- | --- | --- |
| `GW_OCI_NOT_AUTHORIZED` | 403 | `oci_not_authorized` | non_retryable | ctr-functions-api v1 |
| `GW_BASE_DISABLED` | 409 | `base_disabled` | non_retryable | ctr-functions-api v1 |
| `GW_BASE_MISMATCH` | 409 | `base_mismatch` | non_retryable | ctr-functions-api v1 |
| `runtime_mode_unsupported` | 409 | `runtime_mode_unsupported` | non_retryable | worker-only |
| `invalid_request` | 400 | `missing_path` or `non_json_body` | non_retryable | worker-only |
| `request_too_large` | 413 | `body_too_large` | non_retryable | worker-only |

`invalid_request` and `request_too_large` follow the functions API's 400 (malformed) and 413 (body
exceeded the size budget) responses. Adding `GW_` equivalents for the worker-only codes to
ctr-functions-api is a follow-up. ctr-functions-api v1 defines no other base error, so no other base
code exists here.

## Legacy envelope and rejected inputs

The legacy `main(params)` fixtures are those of the
[contract inventory](../falcone-contract-inventory.md#legacy-envelope-fixtures), used byte for byte
under `docs/contracts/fixtures/legacy-envelope/`. A valid fixture's `expected` object is exactly the
`result` of a success result.

Three inputs are rejected before invocation, and nothing is invoked for them:

| Input | Rule | Code | Reason |
| --- | --- | --- | --- |
| oversized-body | `__ow_body` is longer than 1048576 bytes (`max-host-to-guest-bytes`); checked first | `request_too_large` | `body_too_large` |
| missing-path | `__ow_method` is present and `__ow_path` is absent | `invalid_request` | `missing_path` |
| non-JSON-body | the content type is `application/json` and `__ow_body` is not parsable JSON | `invalid_request` | `non_json_body` |

All three are non-retryable. The rejection values in the inventory fixtures are labels, not wire
codes; this table is their mapping.

## Capability grant versions

The credential claims keep capability-grants v1 (`@0.1.0`) while the request projection uses the revision runtime manifest grants (`@0.2.0`).
A credential with a `0.2.0` grant is invalid, and a request with a `0.1.0` grant is invalid. The
request grants match `worker/wit/host.wit` (`falcone:host@0.2.0`). Bumping falcone's
capability-grants to `0.2.0` is a follow-up.

The vendored falcone schemas are in [vendor](vendor/README.md), byte for byte, with their source
revision and sha256. The credential claims schema still references
`https://falcone.dev/contracts/serverless/capability-grants/v1#/$defs/grant` unchanged.

## Error output

No error type, `Debug` or `Display` output, or log line printed for a request echoes a
request-controlled string: not an activation id, attempt id or revision, and not a credential
claim, a projection value or a parameter. Errors carry only static codes and categories.
The serialized refusal keeps every contract field, values included; only formatted text is redacted.

## Conformance

- `worker/src/api` holds the typed messages and the runtime parsers (`parse_request`,
  `parse_update`, `parse_cancel`, `parse_result`). They use serde types and explicit checks and
  never evaluate JSON Schema. `jsonschema` is a test-only dependency.
- `worker/tests/fixtures/contract/` holds the positive and negative fixtures and `index.json`, which
  records each fixture's schema verdict and parser verdict.
- `worker/tests/execution_api_contract.rs` validates every fixture against the full schemas with all
  references resolved offline, checks that the runtime parser accepts exactly what the schemas
  accept (a schema-valid request for a mode other than `wasm` is refused), and checks the failure
  codes, the legacy fixtures, the identity rules, the grant versions, the vendored hashes and the
  redaction rule. None of it executes guest code.

## Sources

ctr-functions-api v1 and ctr-execution-credential v1 are published in gntik-ai/falcone at
`a19f40ab9e160cd522c110efd50c30b2f5e0d708`: `docs/contracts/serverless/functions-api/openapi.v1.yaml`
(sha256 `03121286b0c7ec716304d51a41b866477f4fc21671d95ebeafb8bf9da70e8960`) and
`docs/contracts/serverless/execution-credential/execution-credential.v1.claims.schema.json`
(sha256 `9c98ba16548e724004fbf17c38d3d689fd7183d25b386c83e61b13ff8b31ecf0`).

## Out of scope

OCI execution, the shim transport and OCI-only projection fields (OCI-04), drain semantics and
limit-exceeded details (RUN-02), and mapping ctr-revision-record onto the projection (INV-01,
Falcone). Later changes are additive minor revisions or a new major version.
