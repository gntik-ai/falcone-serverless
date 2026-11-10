# ctr-worker-execution-api: base specification v1

Contract: ctr-worker-execution-api (base)
Version: 1

This is the base worker execution API for RUN-01.t1 / FR-RUN-01 / FR-RUN-02 (design
§2 RUN, §4 RUN-01) under [ADR 0010](../../adr/0010-oci-execution-mode-and-base-catalog.md)
D1. It is the contract the [OCI profile](oci-profile.md) refers to as absent: that
profile and the OCI-04 additions extend this document and may only add optional
fields and codes. A field added as required, or any removal or rename, is a new major
version.

The contract is inert until the dispatcher adopts it. It defines shapes and their
validation; it executes no guest code, adds no OCI execution and makes no cluster call.
ADR 0006 (pool identity and credential format) and ADR 0009 (Falcone owns revision and
activation state) are **Proposed**: v1 follows `ctr-execution-credential` and assumes
Falcone stays authoritative, and it does not claim either ADR is accepted.

The Rust types are in [worker/src/api/](../../../worker/src/api/). Conformance is
[worker/tests/execution_api_contract.rs](../../../worker/tests/execution_api_contract.rs)
over [worker/tests/fixtures/contract/](../../../worker/tests/fixtures/contract/).

## Conventions

- JSON, `snake_case` field names, UTF-8. Every v1 object is closed: an unknown field is
  rejected. Optional fields are omitted when absent; an explicit `null` is malformed.
- Identifiers (`Id`) are 1–256 bytes without control characters. Digests are `sha256:`
  followed by exactly 64 lowercase hex digits. `deadline` is an RFC 3339 UTC timestamp
  ending in `Z`.
- Error messages never echo input values, because an input may hold a credential. An
  unknown field, an unknown variant and a type or value mismatch are reported as fixed
  text, never with the offending key or value.
- Fixtures contain only synthetic values and no credential.

## Execute attempt

The dispatcher sends one attempt to the worker.

| Field | Meaning |
| --- | --- |
| `activation_id` | Activation the attempt belongs to. |
| `attempt_id` | This attempt; a retry has a new `attempt_id`. |
| `invocation` | Invocation identity: `invocation_key`, `attempt_id`, `idempotency_key`, `correlation_id`. `attempt_id` equals the top-level one. |
| `projection` | The pinned revision projection below. |
| `credential` | `{token, claims}`; see Execution credential. |
| `deadline` | The attempt's wall-clock deadline. |
| `params` | JSON object passed to `main(params)`. A legacy function receives the legacy request envelope. |

The attempt is rejected as malformed unless `credential.claims.activation` and
`.attempt` equal `activation_id` and `attempt_id`, `credential.claims.revision` equals
`projection.revision`, and every `credential.claims.allowed_capabilities` entry is in
`projection.granted_capabilities`: a credential may narrow the revision's grants and
never widen them.

### Revision projection v1

| Field | Required | Meaning |
| --- | --- | --- |
| `revision` | yes | Function revision id. |
| `runtime_mode` | yes | `wasm` or `oci`. A projection without it is malformed. |
| `base` | no | The base union below; omitted when the revision pins no base. |
| `artifact_digest` | yes | Digest of the raw validated artifact. |
| `component_digest` | yes | Digest of the component compiled from it. |
| `language_profile` | yes | `{name, version}`, an exact profile pin. |
| `engine_version` | yes | Exact Wasmtime version. |
| `wasi_version` | yes | Exact WASI version. |
| `interface_version` | yes | The `falcone:host` interface version. |
| `declared_imports` | yes | Imports of the validated artifact; empty means none. |
| `budgets` | yes | `{fuel, memory_bytes, timeout_ms, max_host_calls}`, all positive integers. |
| `granted_capabilities` | yes | Verified capability grants. Each is an opaque JSON object: `ctr-capability-grants` v1 is not pinned here. |

The projection is shaped like the [revision runtime manifest](../revision-runtime-manifest.md);
mapping `ctr-revision-record` onto it is INV-01 in Falcone. The base-kind rules
(catalog membership and lifecycle, the BYO flag, OCI authorization) are Falcone's
validation before any write and are not re-decided here. How an `oci` revision fills the
Wasm-oriented fields, and the OCI-only fields (function image digest, container contract
and shim versions), are for OCI-04 to add.

### Base union

`base` is a tagged union on `kind`:

| `kind` | Fields (all required) |
| --- | --- |
| `catalog` | `id`: `name@version`; `digest`: the base image digest. |
| `byo` | `image_digest`; `signer_identity`; `attestation_ref`. |

A projection that pins no base omits `base`. An unresolved default alias is malformed,
never a base: a catalog `digest` is always required, the `id` must carry a version that
starts with a digit (so a version-less id and `latest` are rejected) and its name may not
be `default`. An unknown `kind`, a missing variant field and a field of the other variant
are rejected.

### Runtime mode on the Wasm worker

The Wasm worker reads `projection.runtime_mode` first. Any string other than `wasm`
(`oci`, or a value this contract does not know) is **refused**, whatever else the
projection carries, so fields another mode adds later cannot turn the refusal into a
parse error. The worker executes nothing and answers with the refusal result
(`runtime_mode_not_wasm`, `non_retryable`), whose identity describes the pinned revision:
the revision, the raw `artifact_digest`, the `language_profile`, the `base_digest` exactly
when a base is pinned, and the requested mode when it is `oci`; `wasmtime_version` and
`interface_version` are the worker's own. A missing or non-string `runtime_mode` is
malformed, not a refusal; so is a refused projection without `artifact_digest` or
`language_profile`, or with a pinned base whose kind is unknown, whose digest is missing
or malformed, or whose catalog id is an unresolved alias.

## Execution credential

`credential.claims` binds to `ctr-execution-credential` v1: exactly the properties of
`execution-credential.v1.claims.schema.json`, all required and none added: `iss`, `aud`,
`tenant`, `workspace`, `pool_profile`, `revision`, `activation`, `attempt`, `iat`, `exp`
and `allowed_capabilities`. `credential.token` is the signed token. The worker trusts
the claims only after verifying the token as that contract defines; v1 here checks only
that the claims bind to the request. The token is not carried as an opaque string alone.

The Rust credential type redacts the token in `Debug` and `Display` (`<REDACTED>`). An
implementation must not log an attempt, its credential or its serialized form.

## Execution updates

The worker reports progress while an attempt runs. Each update is
`{activation_id, attempt_id, sequence, update}`; `sequence` starts at 1 and strictly
increases per attempt.

| `update.type` | Fields |
| --- | --- |
| `heartbeat` | none |
| `progress` | `percent` 0–100; optional `message` |
| `checkpoint` | `checkpoint_ref`: a reference to the [checkpoint object](../checkpoint-object.md), never inline state |

## Cancel

`{activation_id, attempt_id, state}` with `state`:

| `state` | Meaning |
| --- | --- |
| `requested` | The dispatcher asks the worker to stop the attempt. |
| `acknowledged` | The worker has stopped the attempt, or has taken it and will report the result. |
| `uncertain` | The worker cannot confirm the guest stopped. Its side effects are unknown; callers treat it as the `uncertain` class, never as success. |

## Result

The result keeps the legacy `{status, result, logs}` envelope:

| Field | Meaning |
| --- | --- |
| `activation_id`, `attempt_id` | The attempt answered. |
| `status` | `success` or `failure`. |
| `result` | JSON object; present exactly when `status` is `success`. A legacy function's result is the legacy result envelope. |
| `logs` | Array of log lines; required, possibly empty. |
| `failure` | `{code, class, message?}`; present exactly when `status` is `failure`. |
| `identity` | The activation runtime identity; always required. |

### Failure classes and codes

Classes are `retryable`, `non_retryable` and `uncertain`. The class of a code never
depends on the attempt: a retry is as non-retryable as the first attempt. A result whose
`class` differs from its code's class is malformed.

Failure code names and meanings come from `ctr-functions-api` v1 and are referenced, not
redefined: the code is the lowercase `detail.reason` of the matching `GW_<X>` code.
`runtime_mode_not_wasm` is the only code defined here.

| Code | Class | Source |
| --- | --- | --- |
| `oci_not_authorized` | non_retryable | ctr-functions-api v1 `GW_OCI_NOT_AUTHORIZED` |
| `base_disabled` | non_retryable | ctr-functions-api v1 `GW_BASE_DISABLED` |
| `base_mismatch` | non_retryable | ctr-functions-api v1 `GW_BASE_MISMATCH` |
| `runtime_mode_not_wasm` | non_retryable | worker-only refusal (above) |

`ctr-functions-api` v1 lists only these three reasons in `failureReason`, so v1 has no
further base error codes; one added there needs a reviewed change here. The legacy request
rejections (`envelope-invalid`, `body-too-large`, `body-not-json`) describe a malformed
request that is rejected before invocation; they are labels, not failure codes.

### Activation runtime identity

Every result carries `identity`:

| Field | Meaning |
| --- | --- |
| `function_revision` | Always required. |
| `runtime_mode` | `wasm` or `oci`. Always required, except that a refusal of an unrecognised mode omits it. |
| `artifact_digest` | Raw artifact digest. Always required. |
| `base_digest` | The digest of the pinned base; present **exactly when** the revision pins a base (catalog `digest`, or BYO `image_digest`). |
| `wasmtime_version` | Wasmtime version that ran the attempt; a refusal reports the worker's own. Always required. |
| `language_profile` | `{name, version}`. Always required. |
| `interface_version` | `falcone:host` interface version; a refusal reports the worker's own. Always required. |

The same fields are required for every result, refusals and `oci` results included; OCI-04
adds further fields additively. Each must equal the projection's value, except that a
refusal's `wasmtime_version` and `interface_version` are the worker's own. A refusal never
reports `wasm`. A result whose `base_digest` is missing for a pinned base, present with no
base, or different from the pinned digest does not describe its attempt, on every path.

## Legacy envelope and invocation identity

The legacy `main(params)` request and `{statusCode, headers?, body?}` result envelopes and
the invocation identity are the MIG-01.t3 fixtures of the
[contract inventory](../falcone-contract-inventory.md) (v1.1, "Legacy envelope fixtures").
They are consumed in place under `docs/contracts/fixtures/legacy-envelope/`, with no copy
and no edit, and each file's sha256 must equal its inventory row.

- Valid fixtures round-trip through the legacy `params` and result types.
- Invalid fixtures are rejected before invocation: a missing `__ow_path` or an unknown
  `__ow_*` parameter is `envelope-invalid`; an `__ow_body` longer than
  `max-host-to-guest-bytes` (1048576 bytes) is `body-too-large`; a body that is not JSON
  under a `content-type` of `application/json` is `body-not-json`.
- The invocation identity maps to `falcone:host/invocation-context`
  (`invocation-key`, `attempt-id`, `idempotency-key`) and the `X-Correlation-Id` header. The
  `Idempotency-Key` header and the `idempotency-key` identity must agree.

## Pinned sources

All sources are in gntik-ai/falcone at revision `a19f40ab9e160cd522c110efd50c30b2f5e0d708`.
The error fixtures and the claims schema are pinned as byte-exact copies under
`worker/tests/fixtures/contract/functions-api/`; the OpenAPI file is recorded by hash only.
Re-pinning needs a reviewed change.

| Source path | Revision | sha256 | Use |
| --- | --- | --- | --- |
| docs/contracts/serverless/functions-api/openapi.v1.yaml | a19f40ab9e160cd522c110efd50c30b2f5e0d708 | 03121286b0c7ec716304d51a41b866477f4fc21671d95ebeafb8bf9da70e8960 | ctr-functions-api v1; hash only |
| tests/contracts/functions-api/fixtures/errors/base_disabled.json | a19f40ab9e160cd522c110efd50c30b2f5e0d708 | 9ed46a38dca56af8f9b9fdc16a413fec45f48a4b1324de2d9a400ed31c49c7d8 | code parity |
| tests/contracts/functions-api/fixtures/errors/base_mismatch.json | a19f40ab9e160cd522c110efd50c30b2f5e0d708 | 2ac0fa4c05c628d6a251657de9918564652d32af9473502de510eaedfdcfa92c | code parity |
| tests/contracts/functions-api/fixtures/errors/oci_not_authorized.json | a19f40ab9e160cd522c110efd50c30b2f5e0d708 | ef0763c3f590a7e5fb31b59073e240d5320681b0f70f13757327cdb7bb26d740 | code parity |
| docs/contracts/serverless/execution-credential/execution-credential.v1.claims.schema.json | a19f40ab9e160cd522c110efd50c30b2f5e0d708 | 9c98ba16548e724004fbf17c38d3d689fd7183d25b386c83e61b13ff8b31ecf0 | claims binding |

## Conformance

`cargo test --workspace --locked` runs, without executing any guest:

- `runtime_mode` required; `oci` and unknown values refused with the exact refusal
  fixture, and OCI-only extra fields refused rather than rejected;
- catalog, BYO and absent base fixtures round-trip as JSON values; unresolved aliases,
  unknown kinds, missing variant fields and extra fields are rejected;
- every failure code has a class here and a fixture; `oci_not_authorized` and
  `base_disabled` are non-retryable on the first attempt and on every retry;
- the pinned error fixtures match their sha256, and the worker code set equals their
  `detail.reason` values plus `runtime_mode_not_wasm`;
- the legacy fixtures match the inventory sha256, valid ones round-trip and invalid ones
  are rejected before invocation;
- the result requires the identity block, with `base_digest` exactly when a base is
  pinned; the credential claims equal the pinned claims schema and the token is redacted.

## Versioning

Later fields are additive and optional. Required new fields, removed or renamed fields
and changed meanings are v2. The OCI profile, drain semantics and limit-exceeded details
(RUN-02) extend v1 additively.
