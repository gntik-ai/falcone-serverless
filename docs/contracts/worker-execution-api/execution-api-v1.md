# ctr-worker-execution-api: base execution API v1

Contract: ctr-worker-execution-api (base API)
Version: 1

This defines RUN-01.t1 / FR-RUN-01, FR-RUN-02 from the
[design](../../falcone-serverless-design.md) and
[ADR 0010](../../adr/0010-oci-execution-mode-and-base-catalog.md) (D1). It is the base
API that the [OCI profile](oci-profile.md) extends; that profile and its fixtures are
unchanged. This is a contract and its Rust types only. It adds no dispatcher, transport,
OCI execution or runtime enablement, and the Rust types execute no guest code.

v1 fields are frozen. A later minor revision may only add optional fields; a required
field, a removed field or a changed meaning needs a new major version and consumer tasks.
INV-01 (mapping `ctr-revision-record` onto the projection, in Falcone), RUN-02 (drain and
limit-exceeded details) and OCI-04 (OCI-only projection fields) extend v1 additively.

Assumptions, not accepted decisions: [ADR 0006](../../adr/0006-pool-identity-and-openbao.md)
(the execution credential format follows `ctr-execution-credential`) and
[ADR 0009](../../adr/0009-schema-migration-ownership.md) (Falcone stays authoritative for
revision and activation state) are Proposed.

## Normative rules

| Rule | Requirement |
| --- | --- |
| E1 | `projection.runtime_mode` is required and is `wasm` or `oci`. The Wasm worker refuses every value other than `wasm` with `runtime_mode_not_wasm` (non-retryable) before it reads any other projection field except the identifiers. A missing or non-string `runtime_mode` is malformed. |
| E2 | `projection.base` is a tagged union and is absent when the revision pins no base. `kind: catalog` carries `id` as `<id>@<version>` and a `digest`; `kind: byo` carries `image_digest`, `signer_identity` and `attestation_ref`. An unresolved default alias (an `id` without `@<version>`, a version that does not start with a digit, a string instead of an object) and an unknown `kind` are malformed. `null` is not absent. |
| E3 | Every digest is `sha256:` followed by 64 lowercase hex digits. |
| E4 | The execution credential is the structured `ctr-execution-credential` v1 claims, never an opaque string. Its `activation`, `attempt` and `revision` claims must equal the request's `activation_id`, `attempt_id` and `projection.revision`. Signature, issuer, audience and time validity are checked by the host capability layer, not by admission. |
| E5 | `invocation.identity.attempt-id` equals `attempt_id`, and `invocation.identity.idempotency-key` equals the `Idempotency-Key` header. |
| E6 | The legacy params are rejected before invocation when `__ow_path` is absent, when `__ow_body` exceeds 1048576 bytes (`max-host-to-guest-bytes`), or when a body under a JSON `content-type` is not JSON. All three are local rejections before invocation, mapped to the worker rejection codes under Legacy rejections: no wire failure code is defined for them and no result is produced. |
| E7 | Every result of an admitted attempt carries the activation runtime identity block, and the identity must equal the request's projection. `base_digest` is present exactly when the projection pins a base, and equals the catalog `digest` or the BYO `image_digest`. The result parsing entry point enforces this in every build. |
| E8 | A failure's `class` is the class of its `code` (table below). The class never depends on the attempt number: a retry of a non-retryable code is still non-retryable. |
| E9 | No formatted error text, log line or message prints a request field. `activation_id`, `attempt_id`, `projection.revision` and every other request string appear only as `<redacted>`; request-derived enums and validated codes may be printed. Serialized results keep their contract fields. Serde error text is never forwarded, because it echoes input values. |

## Execute attempt

The dispatcher sends one JSON object per attempt. Unknown fields are rejected.

| Field | Type | Meaning |
| --- | --- | --- |
| `contract_version` | integer | Must be 1. |
| `activation_id` | string | Activation identifier. |
| `attempt_id` | string | Attempt identifier; a retry has a new one. |
| `deadline_unix_ms` | integer | Deadline in epoch milliseconds, in the dispatcher's clock domain. |
| `execution_credential` | object | `ctr-execution-credential` v1 claims: `iss`, `aud`, `tenant`, `workspace`, `pool_profile`, `revision`, `activation`, `attempt`, `iat`, `exp`, `allowed_capabilities` (capability-grants v1 grants, opaque here). |
| `projection` | object | The pinned revision projection. |
| `invocation` | object | `headers` (`X-Correlation-Id`, `Idempotency-Key`) and `identity` (`invocation-key`, `attempt-id`, `idempotency-key`). None is a credential. |
| `params` | object | The flat legacy `main(params)` object: `__ow_*` parameters beside the action parameters (inventory v1.1). |

### Revision projection v1

| Field | Type | Meaning |
| --- | --- | --- |
| `revision` | string | Function revision. |
| `runtime_mode` | `wasm` or `oci` | Required. |
| `base` | object | Optional tagged union; absent when no base is pinned. |
| `artifact_digest` | digest | Raw artifact digest. |
| `component_digest` | digest | Validated component digest. |
| `language_profile` | string | Language profile. |
| `engine_version` | string | Wasmtime version the revision is pinned to. |
| `wasi_version` | string | WASI version. |
| `interface_version` | string | Host interface version. |
| `imports` | list of strings | Declared imports. |
| `budgets` | object | `guest_memory_bytes`, `guest_to_host_bytes`, `host_to_guest_bytes`, `guest_deadline_ms`, `host_call_deadline_ms`, `termination_budget_ms`; all positive integers. |
| `granted_capabilities` | list of strings | Granted capabilities. |

A non-`wasm` projection need carry only `revision`, `runtime_mode` and whatever else the
sender has; the Wasm worker does not interpret the rest (E1). OCI-only fields, such as the
function image digest and the container contract and shim versions, are added by OCI-04.

## Updates and cancel

Updates flow from the worker while an attempt runs. Each names the attempt's
`activation_id` and `attempt_id`, which must match the attempt, and has `kind`:

- `heartbeat`: `sequence` (integer, increasing within the attempt).
- `progress`: `sequence` and `permille` (0 to 1000).
- `checkpoint`: `sequence` and `checkpoint_ref`, a reference to a committed checkpoint
  object ([checkpoint-object.md](../checkpoint-object.md)).

Cancel is one object, `activation_id`, `attempt_id` and `state`:

| State | Sent by | Meaning |
| --- | --- | --- |
| `requested` | dispatcher | The attempt should stop. |
| `acknowledged` | worker | The guest is confirmed stopped. |
| `uncertain` | worker | The worker cannot confirm the guest stopped; side effects may exist and the attempt must not be blindly replayed. |

## Result

The result keeps the legacy `{status, result, logs}` envelope and adds `activation_id`,
`attempt_id`, `failure` and the required `identity` block.

| Field | Meaning |
| --- | --- |
| `status` | `success`, `application_error` or `failure`. |
| `result` | The handler's legacy envelope `{statusCode, headers?, body?}`. Required for `success`, absent for `failure`. A 304 has no `body`. |
| `logs` | List of log lines. |
| `failure` | `{code, class}`. Present exactly when `status` is `failure`. |
| `identity` | The activation runtime identity block. |

Activation runtime identity block: `revision`, `runtime_mode`, `artifact_digest`,
`base_digest` (only when a base is pinned), `wasmtime_version`, `language_profile`,
`interface_version`.

A refusal is produced before the projection is interpreted, so it has no identity block.
Its fields are `activation_id`, `attempt_id`, `revision`, `requested_runtime_mode` (`oci`,
or `unrecognized` for any other value), `status` (`failure`), `logs` (empty) and `failure`.

## Failure codes

The codes are the OCI and base family. Their names and meanings are taken from
ctr-functions-api v1 and referenced, not redefined: `detail.reason` of the gateway error
equals the code. The only code that exists in this contract alone is
`runtime_mode_not_wasm`.

| Code | Family | Class | ctr-functions-api code | Pinned fixture |
| --- | --- | --- | --- | --- |
| oci_not_authorized | oci_base | non_retryable | GW_OCI_NOT_AUTHORIZED | oci_not_authorized.json |
| base_disabled | oci_base | non_retryable | GW_BASE_DISABLED | base_disabled.json |
| base_mismatch | oci_base | non_retryable | GW_BASE_MISMATCH | base_mismatch.json |
| runtime_mode_not_wasm | oci_base | non_retryable | worker-only | none |

Every v1 code is `non_retryable`. The other two classes are part of the result vocabulary
for later additive revisions (RUN-02 drain and limit details); no v1 code uses them.

- `retryable`: no guest ran; the dispatcher may retry.
- `non_retryable`: a retry cannot succeed, on the first attempt or any later one.
- `uncertain`: the guest may have run; the dispatcher must not blindly replay it.

`oci_not_authorized` and `base_disabled` are non-retryable on every path, including
retries. `runtime_mode_not_wasm` is the Wasm worker's refusal of a non-`wasm` mode.

### Legacy rejections

The invalid fixtures of inventory v1.1 carry fixture labels, not wire codes. Each is
rejected before invocation (`ApiError::Rejected`) and mapped to a worker rejection code.
A rejection code is local to the worker: no attempt runs and no result is produced, so it
is never a `failure.code`, is not in the failure code table above and has no
ctr-functions-api counterpart. It is `non_retryable`: the same envelope cannot succeed on
a retry. The worker-only exception for failure codes covers the OCI and base failure family; these rejection codes are a separate local set.

| Fixture label | Worker rejection code | Class | Reason |
| --- | --- | --- | --- |
| envelope-invalid | legacy_envelope_invalid | non_retryable | `__ow_path` is absent |
| body-too-large | legacy_body_too_large | non_retryable | `__ow_body` is over the limit |
| body-not-json | legacy_body_not_json | non_retryable | a JSON body is not parsable |

## Consumed sources

All sources are in gntik-ai/falcone at revision `a19f40ab9e160cd522c110efd50c30b2f5e0d708`.
The worker sandbox cannot read that repository, so copies of the error fixtures are pinned
in `worker/tests/fixtures/contract/pinned/`, recorded with path and sha256 in
`worker/tests/fixtures/contract/pinned-sources.json`. Their gateway codes and
`detail.reason` values are the ones in the table above.

| Source | sha256 |
| --- | --- |
| docs/contracts/serverless/functions-api/openapi.v1.yaml | 03121286b0c7ec716304d51a41b866477f4fc21671d95ebeafb8bf9da70e8960 |
| docs/contracts/serverless/execution-credential/execution-credential.v1.claims.schema.json | 9c98ba16548e724004fbf17c38d3d689fd7183d25b386c83e61b13ff8b31ecf0 |
| tests/contracts/functions-api/fixtures/errors/base_disabled.json | 9ed46a38dca56af8f9b9fdc16a413fec45f48a4b1324de2d9a400ed31c49c7d8 |
| tests/contracts/functions-api/fixtures/errors/base_mismatch.json | 2ac0fa4c05c628d6a251657de9918564652d32af9473502de510eaedfdcfa92c |
| tests/contracts/functions-api/fixtures/errors/oci_not_authorized.json | ef0763c3f590a7e5fb31b59073e240d5320681b0f70f13757327cdb7bb26d740 |

The legacy envelope and invocation-identity fixtures are those of the
[contract inventory](../falcone-contract-inventory.md) v1.1, used byte for byte from
`docs/contracts/fixtures/legacy-envelope/` with the sha256 values the inventory records.

## Conformance

`worker/tests/execution_api_contract.rs` runs the conformance fixtures in
`worker/tests/fixtures/contract/` through the types in `worker/src/api/`. The tests
execute no guest code. Passing them shows agreement with these fixtures, not runtime
enforcement: no dispatcher calls the API yet, and the dispatcher issue that first does
removes the dead-code allowance on `mod api;` in `worker/src/main.rs`.
