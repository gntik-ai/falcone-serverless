# Frozen Falcone contract inventory

Contract version: v1.2

This freezes the v1 compatibility surfaces and evidence levels for MIG-01, not
deployed acceptance or an enumeration of every live endpoint. No live deployment
inspection was performed. Source-defined rows refer to the Falcone snapshot
`bde62dd1e2e91f957b1dc6b8212dfbbaee977a7f` cited by the design; they need
revalidation before cutover. Proposed rows are requirements awaiting discovery or
qualification. API parity alone never establishes execution compatibility.

## Contract surfaces

| ID | Contract | Status | Source |
| --- | --- | --- | --- |
| routes | Preserve inventoried Falcone Functions routes through the compatibility adapter; exact route discovery remains required. | proposed | design §2 API |
| api-versions | Preserve supported Falcone API versions and qualify generated clients against each version. | proposed | design §2 API |
| schemas | Preserve request/response schemas and publish machine-readable OpenAPI schemas. | proposed | design §2 API |
| error-codes | Preserve inventoried error codes, including tenant-safe runtime outage and cleanup behavior. | proposed | design §2 API; S9 |
| pagination | Preserve inventoried pagination behavior; do not infer it from example payloads. | proposed | design §2 API |
| idempotency | Preserve inventoried idempotency behavior; uncertain side effects must not be blindly replayed. | proposed | design §2 API; design §3 Synchronous invocation |
| urls | Preserve Falcone-owned function URLs, including the source-defined runtime-status and cleanup boundary. | source-defined | S9; design §3 Migration and compatibility |
| data | Retain public function identifiers, ownership, schemas and activation history; pin revisions and classify dependencies before migration. | proposed | design §3 Migration and compatibility |
| main-params-envelope | The source executor accepts main(params) and returns the status/result/logs envelope; qualify legacy behavior per supported profile. | source-defined | S2; design §2 RUN |
| functions-invoke | Preserve the Temporal functions.invoke Activity's workspace binding, revision pins and failure mapping. | source-defined | S3 |
| authorization | A project is a Falcone workspace; preserve tenant/workspace identity, role/plan and scoped invocation checks. OCI requires superadmin authorization of that workspace at cutover. | source-defined | S1; S3; ADR 0010 |
| websocket | Preserve required WebSocket authentication/reconnect and delivery semantics; deployed behavior still needs discovery and qualification. | proposed | design §0 Confirmed requirements; design §3 WebSockets, event delivery and scale |
| mcp | Discover and preserve actually deployed MCP contracts if found; deployed MCP behavior is unconfirmed and speculative transports are outside mandatory scope. | proposed | design §3 Migration and compatibility |

## Knative exclusion

Raw Knative APIs and Knative Service URLs are outside the replacement scope.
The retained URL contract covers Falcone-owned URLs only. Existing Knative may
serve as an optional migration reference; a clean installation has no Knative
dependency. This boundary does not waive function compatibility classification.

## Classification vocabulary

Every existing function must be classified at its pinned immutable revision
before migration. Record these dimensions (design §3 Migration and compatibility,
design §3 Technology and deployment model, and ADR 0010):

- language
- packages/package manager
- native libraries or binaries
- Dockerfile/OCI image: feeds oci_verdict; not an automatic blocker.
- install hooks/build scripts
- process/OS interfaces
- Knative API/URL use

Verdicts: `candidate`, `needs-port`, `unsupported`, `unclassified`.

`candidate` means qualification evidence supports that mode at the pinned
revision; it is not an API-parity inference. `needs-port` requires changes and
requalification; `unsupported` has no demonstrated supported path;
`unclassified` has no completed classification. Record both mode verdicts even
when one mode cannot run the workload. A Dockerfile or OCI image is evidence for
OCI qualification, not proof of catalog eligibility, BYO permission or execution
authorization. Wasm restrictions remain in force; Falcone never runs tenant
Dockerfiles or install hooks to make a function compatible.

Required fields: `function-id`, `function-revision`, `wasm_verdict`, `oci_verdict`, `target_mode`.

target_mode values: `wasm|oci`.

`wasm_verdict` and `oci_verdict` each use the verdict vocabulary above.
target_mode is never derived from a failed wasm verdict.
The target is an explicit revision choice (Wasm by default); selecting OCI
requires an authorized deployment of a new revision, never automatic fallback.

## Fail-closed rule

Evaluate all blockers before returning candidate, regardless of API parity.
No classification means no record for the function; a revision mismatch means
the classified function-revision differs from the pinned revision at cutover.
Select wasm_verdict for target_mode wasm and oci_verdict for target_mode oci.
For target_mode oci, the workspace must be OCI-authorized at cutover.
Only a candidate verdict for the selected mode at the pinned revision, with
every applicable blocker absent, permits cutover. Candidate for the other mode
does not satisfy this gate. This documents the MIG-02 guard contract; it does
not implement migration, backend selection or OCI enforcement.

| Condition | Outcome |
| --- | --- |
| no-classification | blocker |
| revision-mismatch | blocker |
| verdict-not-candidate | blocker |
| oci-not-authorized | blocker |
| candidate-at-pinned-revision | candidate |

The only outcomes are blocker and candidate. Under ADR 0010, oci-not-authorized
applies when the target is oci and the workspace lacks OCI authorization at
cutover; a candidate OCI verdict cannot waive it. Unknown, incomplete or
non-candidate classifications cannot authorize migration or select another mode.

## Legacy request envelope

Revision v1.1 adds the legacy envelope fixtures that #124 and #126 bind to
byte for byte (MIG-01.t3). The v1 sections above are unchanged. Legacy handlers
are called as `main(params)` (row main-params-envelope). The OpenWhisk-style
request envelope is the flat `params` object:

| Parameter | Meaning |
| --- | --- |
| `__ow_method` | Lowercase HTTP method, for example `get` or `post`. |
| `__ow_headers` | Object of request headers; the reference function reads lowercase names. |
| `__ow_path` | Request path; required. The reference function extracts the workspace ID from `/v1/workspaces/{id}/openapi`. |
| `__ow_query` | Object of query parameters; the reference function reads `format`. |
| `__ow_body` | Request body as text; present only when the request carries a body. |

Action parameters sit beside the `__ow_*` parameters in the same object.

## Legacy result envelope

The handler returns `{statusCode, headers?, body?}`: `statusCode` is an
integer HTTP status, `headers` an optional object of response headers and `body`
an optional JSON value or text. A 304 result has no `body`.

The reference function evaluates the request in this order and the first match
decides the result: auth (tenant and requester headers) 401, path 400, rate
limit 429, spec lookup 404, tenant 403, ETag 304, format 200 json or yaml. The
ETag is `"sha256:<hex>"` with the quotes (spec-cache.mjs). Format is yaml when
`__ow_query.format` is `yaml` or the `accept` header is `application/x-yaml`.

## Invocation identity

The invocation identity maps to the `falcone:host/invocation-context` functions
`invocation-key`, `attempt-id` and `idempotency-key` (worker/wit/host.wit).
`idempotency-key` is the `Idempotency-Key` header of the invoke request
(invokeFunctionAction.json); `invocation-key` and `attempt-id` are synthetic in
the fixtures because the pinned sources do not define them. The `X-Correlation-Id`
header travels with the invoke request. None of these identifiers is a credential.

## Legacy envelope fixtures

Fixtures are exact bytes with LF line endings and one final newline; each sha256
is the lowercase hash of those bytes. Each valid fixture records the request
`params`, the synthetic `dependencyState` needed to reach its path (rateLimit
limit, now and prior bucket count; the getCurrentSpec result), the invoke
headers with the invocation identity, and the `expected` result envelope that
the reference code produces. Each invalid fixture is expected to be rejected
before invocation.

| Fixture | sha256 | Falcone source | Revision |
| --- | --- | --- | --- |
| docs/contracts/fixtures/legacy-envelope/valid/401.json | 3ed82379573d4d0edf7a1b9a9d4b3cce47640a29bed85df3397ff8ba4e4854ff | packages/openapi-sdk-service/actions/openapi-spec-serve.mjs | 10a3c6e7a23c9a29b934c35f0682bbdef232099f |
| docs/contracts/fixtures/legacy-envelope/valid/400.json | 0f09bdb42b0ce1ba49ab3e950ea07136491dfaaaac7a65065b25659a7c16b949 | packages/openapi-sdk-service/actions/openapi-spec-serve.mjs | 10a3c6e7a23c9a29b934c35f0682bbdef232099f |
| docs/contracts/fixtures/legacy-envelope/valid/429.json | 27ed6860e27beb0dafb7a144948644c5b6c227f1c7c1f74220fd992b80d0d6c5 | packages/openapi-sdk-service/actions/openapi-spec-serve.mjs | 10a3c6e7a23c9a29b934c35f0682bbdef232099f |
| docs/contracts/fixtures/legacy-envelope/valid/404.json | 33bdfb0ccf1d60571f69a9a8e58a6f0abf96629c1f1c5e419cf14b399a80f63a | packages/openapi-sdk-service/actions/openapi-spec-serve.mjs | 10a3c6e7a23c9a29b934c35f0682bbdef232099f |
| docs/contracts/fixtures/legacy-envelope/valid/403.json | fdcb8c79b726564c3e5433ab1eb851700dcd234cd32db908840fa795605e5ff8 | packages/openapi-sdk-service/actions/openapi-spec-serve.mjs | 10a3c6e7a23c9a29b934c35f0682bbdef232099f |
| docs/contracts/fixtures/legacy-envelope/valid/304.json | 132ae45adf2c663d4ce7ea9bb75ee5e5e56ea5ca76e39a54363419fdf43d9d1b | packages/openapi-sdk-service/actions/openapi-spec-serve.mjs | 10a3c6e7a23c9a29b934c35f0682bbdef232099f |
| docs/contracts/fixtures/legacy-envelope/valid/200-json.json | 0fd272cd853f40cad7b3e1886b1c39c0e0004f4ae146456a95b8ee6244160127 | packages/openapi-sdk-service/actions/openapi-spec-serve.mjs | 10a3c6e7a23c9a29b934c35f0682bbdef232099f |
| docs/contracts/fixtures/legacy-envelope/valid/200-yaml.json | e5afd3a7fd5f6dd1b4c90f58fb3ba40e6abbb2208f1edbafa30031438e5ed7db | packages/openapi-sdk-service/actions/openapi-spec-serve.mjs | 10a3c6e7a23c9a29b934c35f0682bbdef232099f |
| docs/contracts/fixtures/legacy-envelope/invalid/missing-path.json | b61cf55fe179fb4174c58d40c3808d4b461039051d5b99bbdbaa4ebc15e2037a | tests/contracts/functions-api/fixtures/legacy/invokeFunctionAction.json | a19f40ab9e160cd522c110efd50c30b2f5e0d708 |
| docs/contracts/fixtures/legacy-envelope/invalid/oversized-body.json | c98ab3c441140257401123d5e32c3b121a4825eb5b1392552a05d7c9be4879c4 | tests/contracts/functions-api/fixtures/legacy/invokeFunctionAction.json | a19f40ab9e160cd522c110efd50c30b2f5e0d708 |
| docs/contracts/fixtures/legacy-envelope/invalid/non-JSON-body.json | bb28023f7e5605b798847b5aedf913598fc0b2edace94997dfdd8eec1d9227da | tests/contracts/functions-api/fixtures/legacy/invokeFunctionAction.json | a19f40ab9e160cd522c110efd50c30b2f5e0d708 |

The valid fixtures cover exactly these paths: 401, 400, 429, 404, 403, 304, 200-json and 200-yaml.
Valid 400 has a present `__ow_path` that does not match the workspace route; invalid missing-path omits `__ow_path` entirely. They are distinct cases.
The invalid fixtures are missing-path, oversized-body and non-JSON-body.
Size limit source: `max-host-to-guest-bytes` in `worker/wit/host.wit`.
oversized-body is valid JSON of exactly that limit plus one byte, committed literally.
All identifiers in the fixtures are synthetic; the fixtures hold no credentials.
The rejection values are fixture labels, not wire error codes; #124 maps them.

## Legacy envelope source provenance

All sources are in gntik-ai/falcone. Each sha256 is of the file at that revision.

| Source | Revision | sha256 | Role |
| --- | --- | --- | --- |
| packages/openapi-sdk-service/actions/openapi-spec-serve.mjs | 10a3c6e7a23c9a29b934c35f0682bbdef232099f | 3827f5e2c7c14cc3cf5abb4eb8272d8cb67938dc4d195723efcf695bf8924ce1 | reference function; defines every valid fixture |
| packages/openapi-sdk-service/src/config.mjs | 10a3c6e7a23c9a29b934c35f0682bbdef232099f | 749d3fefde37945e57607ae462a6a55ee193181c898ae4bbbd641f1dae0a30e5 | imported helper; default specRateLimitPerMinute 60 |
| packages/openapi-sdk-service/src/spec-version-repo.mjs | 10a3c6e7a23c9a29b934c35f0682bbdef232099f | 5b13e9bd885954900ddf1d4e2d7b780d5e46d73e09074b31bdf5aa013f5de437 | imported helper; getCurrentSpec result shape |
| packages/openapi-sdk-service/src/spec-cache.mjs | 10a3c6e7a23c9a29b934c35f0682bbdef232099f | 74883a947b993384a4fe3fd697e7cf4f73bdb2f36ac688ec4303bea43d17e2dc | imported helper; ETag and contentHash format |
| tests/contracts/functions-api/fixtures/legacy/invokeFunctionAction.json | a19f40ab9e160cd522c110efd50c30b2f5e0d708 | 46d7d7e19f10b8dac1fc561317a9b0d2e012b184647534c58246605a92505664 | invocation headers; defines every invalid fixture |

The directory tests/contracts/functions-api/fixtures/legacy/ at a19f40ab9e160cd522c110efd50c30b2f5e0d708 holds 23
control-plane request fixtures; only invokeFunctionAction.json is used here. The
reference function also imports network.mjs and spec-audit.mjs, which are not
pinned here: the fire-and-forget spec-accessed audit emission on 200 paths is not
part of the result envelope and is not recorded.

## Legacy compatibility notes

- The reference function keeps its rate-limit buckets in a module-level Map that persists across warm invocations. Per-invocation isolation (ADR 0004) changes this. #126 must resolve it; this revision only records it.
- If `__ow_path` is absent the reference function itself answers 400 INVALID_PATH, because `extractWorkspaceId` defaults it to an empty string. The invalid missing-path fixture is a stricter envelope-shape rejection before invocation; it is not a 400 path.
- The pinned sources do not type the format_json column; the fixtures model formatJson as the text that computeContentHash hashes.

## Versioning and evidence

This document is a documentation contract, not runtime policy or live evidence.
Real function classification belongs to MIG-01.t3; profile qualification to
WAS-03; migration enforcement to MIG-02; OCI execution to EP-OCI/EP-CAT.
Additive corrections require a minor contract version; removing or renaming
rows, statuses, verdicts or conditions requires v2 and an updated gate in the
same change. Until such a reviewed change, the cargo test freezes the v1 shape.

## Serverless chart values

Revision v1.2 adds `ctr-serverless-chart-values` v1 (OPS-01.t1). The frozen v1
sections and the v1.1 legacy envelope contract remain unchanged. The schema
requires digest-pinned Docker Hub images, namespace-only configuration and OCI
off by default. Consumers pin v1; breaking changes require v2. This is a
published input contract, not Helm rendering or runtime enablement evidence.

| Contract | Version | Schema | sha256 |
| --- | --- | --- | --- |
| ctr-serverless-chart-values | v1 | docs/contracts/serverless-chart-values/chart-values.v1.schema.json | 563060d0f30968864ff8b6984d0af16aa48176da11b6c9bcbecbf839ea015ed7 |

Field descriptions, provisional replica/pool bounds, OCI requirements and
synthetic valid/invalid fixtures are in
[serverless-chart-values/README.md](serverless-chart-values/README.md). The
inventory test recomputes the lowercase sha256 of the schema bytes.
