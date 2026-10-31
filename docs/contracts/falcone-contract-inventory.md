# Frozen Falcone contract inventory

Contract version: v1

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

## Versioning and evidence

This document is a documentation contract, not runtime policy or live evidence.
Real function classification belongs to MIG-01.t3; profile qualification to
WAS-03; migration enforcement to MIG-02; OCI execution to EP-OCI/EP-CAT.
Additive corrections require a minor contract version; removing or renaming
rows, statuses, verdicts or conditions requires v2 and an updated gate in the
same change. Until such a reviewed change, the cargo test freezes the v1 shape.
