<!-- Negative fixture for controller/pool_status_report_contract_test.go: a copy of the contract with five defects: no observationSeq field, case G4 marked replay_noop, no unbound_namespace refusal rule, tenant-visible poolRef.namespace, and a tenant-visible ImagePullFailed reason that carries a registry path. -->
# Pool status report

Contract: ctr-pool-status-report
contractVersion: 1
contractRevision: 1.2

This is the report a namespaced pool controller sends to Falcone for DEP-03,
implementing FR-DEP-05 (a restarted controller converges on one pool
configuration and routing generation, and stale updates cannot overwrite newer
state). It includes the OCI fields and conditions of
[ADR 0010](../adr/0010-oci-execution-mode-and-base-catalog.md). Falcone gates
traffic on readiness (DEP-02), so this report decides whether a tenant revision
receives traffic: authentication, scoping and ordering here are a tenant
isolation boundary (R3).

This contract defines the report only. It does not implement emission in the
controller or ingestion in Falcone, and it does not define
`ctr-platform-service-identity`, `ctr-namespace-bindings`,
`ctr-runtime-status-model` or `ctr-resource-ownership`; it references them by key.
Report cadence, heartbeat freshness and scaling metrics are out of scope. Additive
changes take a minor `contractRevision`; breaking changes bump the integer
`contractVersion`. Both update the conformance test and reference comparator
(`controller/pool_status_report_contract_test.go`). This follows the additive
versioning policy in [the inventory](falcone-contract-inventory.md).

Revision 1.2 adds the reporter lineage (`reporterLineage`) to the fence; see
[Generation semantics](#generation-semantics) and [Ordering cases](#ordering-cases).
The [v1 baseline schema](pool-status/v1/pool-status.schema.json) fixes the original
contractVersion 1 field table and allows unknown fields for old consumers. The
[v1.1 schema](pool-status/v1.1/pool-status.schema.json) accepts v1 reports without
`contractRevision`, and strictly checks revision `"1.1"` reports. The
[v1.2 schema](pool-status/v1.2/pool-status.schema.json) accepts both of those
unchanged and strictly checks revision `"1.2"` reports, which differ from 1.1 only
by the required `reporterLineage`. Each directory
contains versioned `conformance.json` examples; Go tests validate all three schemas,
all examples and the encoder, and reject removed or retyped v1 fields.
No v1 field or condition is removed or renamed. A bare v1 `absenceVerified=true`
remains valid for compatibility; cleanup consumers must require revision 1.1,
`absenceVerified=true` and its verifiedAbsence evidence together before completing
obligations (TEN-03.t5). Evidence alone is not a completion signal.

## Fields

One report describes one pool. Visibility is `tenant` or `operator`. Visibility
defaults to `operator`; the only tenant-visible field is `runtimeMode`. The
poolRef namespace and name, every Kubernetes name, UID and resourceVersion, and
`conditions` are operator-visible only. What a tenant sees of a pool is its
status and tenant reason from the [Status mapping](#status-mapping) tables.

| Name | Type | Required | Visibility |
| --- | --- | --- | --- |
| `poolRef` | object | yes | operator |
| `poolRef.namespace` | string | yes | tenant |
| `poolRef.name` | string | yes | operator |
| `observedGeneration` | integer | yes | operator |
| `reporterEpoch` | integer | yes | operator |
| `reporterLineage` | string | 1.2 | operator |
| `desiredReplicas` | integer | yes | operator |
| `readyReplicas` | integer | yes | operator |
| `revisions` | array | yes | operator |
| `revisions[].revision` | string | yes | operator |
| `revisions[].ready` | boolean | yes | operator |
| `readyCapacity` | integer | yes | operator |
| `draining` | boolean | yes | operator |
| `conditions` | array | yes | operator |
| `conditions[].type` | string | yes | operator |
| `conditions[].reason` | string | yes | operator |
| `absenceVerified` | boolean | yes | operator |
| `runtimeMode` | `wasm` or `oci` | yes | tenant |
| `revision` | string | oci | operator |
| `materialized` | boolean | oci | operator |
| `contractRevision` | string | no | operator |
| `ownedObjects` | array | no | operator |
| `ownedObjects[].kind` | string | entry | operator |
| `ownedObjects[].namespace` | string | entry | operator |
| `ownedObjects[].name` | string | entry | operator |
| `ownedObjects[].uid` | string | entry | operator |
| `ownedObjects[].resourceVersion` | string | entry | operator |
| `conditions[].objectRef` | object | no | operator |
| `conditions[].objectRef.kind` | string | entry | operator |
| `conditions[].objectRef.name` | string | entry | operator |
| `conditions[].remainingDescendants` | object | TerminationPending | operator |
| `conditions[].remainingDescendants.replicaSets` | integer | entry | operator |
| `conditions[].remainingDescendants.pods` | integer | entry | operator |
| `verifiedAbsence` | array | no | operator |
| `verifiedAbsence[].revision` | string | oci | operator |
| `verifiedAbsence[].generation` | integer | entry | operator |
| `verifiedAbsence[].readAt` | string | entry | operator |
| `verifiedAbsence[].absent` | object | entry | operator |
| `verifiedAbsence[].absent.deployments` | boolean | entry | operator |
| `verifiedAbsence[].absent.services` | boolean | entry | operator |
| `verifiedAbsence[].absent.leases` | boolean | entry | operator |
| `verifiedAbsence[].absent.replicaSets` | boolean | entry | operator |
| `verifiedAbsence[].absent.pods` | boolean | entry | operator |

Notes:

- `Required` is `yes` for every report. `oci` means required when
  `runtimeMode` is `oci` and absent otherwise. `no` means optional. `entry`
  means required whenever its containing entry or object is present.
  `TerminationPending` requires remainingDescendants only for that condition.
  The extension rows apply to contractRevision 1.1 and later; v1 has only the
  original rows through materialized. `1.2` means required in revision 1.2 and
  forbidden in 1.1 and v1. All new fields have operator visibility.
- `revision` is the Falcone revision ID of the single revision an OCI pool
  serves. For `oci`, `revisions` holds exactly the entry named by `revision`.
  `materialized` is true while the pool's Deployment and Service exist and false
  after dematerialization. It is operator-visible.
- `readyReplicas`, `readyCapacity` and `revisions[].ready` are readiness. They
  can change without `observedGeneration` changing.
- `conditions[]` has no free-text message field. Condition types are listed in
  [Repair conditions](#repair-conditions) and [Status mapping](#status-mapping).
  Tenant-visible text comes only from the mapping tables, never from a report field.

### Object identity and cleanup evidence

`contractRevision` is the string `"1.1"` or `"1.2"`; a controller emits `"1.2"`. `ownedObjects` records only verified
pool-scoped Deployments, Services and Leases in the authoritative pool binding.
Each identity contains the UID and resourceVersion observed after the last
controller or scaler write. These are report values only: they never replace the
same-attempt fresh-read preconditions of [resource ownership](resource-ownership.md).
Never compare a reported resourceVersion to a later read as a deletion guard.

Leases in both `ownedObjects` and `absent` mean pool-scoped Leases only, with the
expected pool name. The singleton `falcone-pool-controller-leader` and
`falcone-pool-controller-scaler` Leases are never listed or counted: they are
controller infrastructure and will not become absent during tenant cleanup.

A mismatch never echoes observed values from a possibly foreign object.
`conditions[].objectRef`, when supplied, contains only the expected kind
(Deployment, Service or Lease) and name derived from authoritative pool state.
It has no UID, owner annotation, namespace or observed name. This applies even
when truncated pool hashes collide in a shared namespace. The encoder takes a
trusted Scope (binding, expected name and full owner annotation), ignores input
ownedObjects, filters fresh ownership-verified observations against that scope,
and suppresses all owned object identities while a cleanup condition blocks.
It never sends owner annotations. Scope is not derived from observed objects.
The wire validator checks ownedObjects namespace/name against poolRef and
objectRef names against poolRef.name; JSON Schema cannot express those
cross-field comparisons, so schema validation alone does not replace them.

Revision 1.1 fixes the operator reason codes (these are not tenant text):

| Reason | Type |
| --- | --- |
| uid_mismatch | UIDMismatch |
| resource_version_mismatch | ResourceVersionMismatch |
| ownership_mismatch | OwnershipMismatch |
| missing_preconditions | MissingPreconditions |
| termination_pending | TerminationPending |
| image_pull_failed | ImagePullFailed |
| container_start_failed | ContainerStartFailed |
| contract_probe_failed | ContractProbeFailed |
| dematerialized | Dematerialized |

`TerminationPending` requires nonnegative `remainingDescendants.replicaSets` and
`remainingDescendants.pods` counts with at least one nonzero count. Other
conditions cannot carry remainingDescendants. Conditions still have no free text.

Each `verifiedAbsence` entry identifies the dematerialized entry's generation,
its OCI revision (OCI only), and an RFC 3339 fresh-read time `readAt`. Its `absent`
object must explicitly set all five keys to true: deployments, services, leases,
replicaSets and pods. Missing keys or false values are never absence evidence.
The generation identifies the desired-state entry being cleaned up; the revision
must match the OCI pool's revision. A Deployment being absent alone is not proof
that its descendant ReplicaSets and Pods have stopped running tenant code.

With contractRevision present, absenceVerified=true requires at least one
verifiedAbsence entry and forbids UIDMismatch, ResourceVersionMismatch,
OwnershipMismatch, MissingPreconditions and TerminationPending. Reports without
contractRevision remain v1, including their original absenceVerified semantics.
Either `absenceVerified=true` or a nonempty verifiedAbsence list forbids live
ownedObjects, those blocking conditions, `materialized=true`, nonzero
readyReplicas or readyCapacity, and any ready revision. This holds even when
absenceVerified is false: evidence cannot contradict live pool state.

**Until a reviewed mechanism proves descendant absence, the controller must
report absenceVerified=false.** This change grants no Pod or ReplicaSet reads,
widens no RBAC, and implements no proof, deletion logic or emission path.
TEN-03.t4 must settle descendant proof before any producer asserts it.
The encoder accepts evidence from its trusted caller; validation checks its
shape, not whether the caller actually performed fresh reads.
The encoder refuses either absence signal when any observation in the same input
occupies the expected pool namespace and name, including an object whose owner
annotation is missing or foreign. It refuses without echoing that object's
identity. A mismatch blocks completion; it is never evidence of absence.

The encoder kill switch is `Options.DisableExtensions`. It emits v1 only,
omitting contractRevision, ownedObjects, verifiedAbsence, objectRef and
remainingDescendants, and omitting the new MissingPreconditions and
TerminationPending conditions. It forces absenceVerified=false even when its
input says true. Wiring this option to controller configuration belongs to
TEN-03.t4; ctr-controller-config and preflight are unchanged.

## Transport

The controller sends each report to Falcone as an in-cluster HTTP request. The
design defers gRPC, NATS and AMQP transports, so this version does not use them.
The wire encoding of the fields above is JSON.

## Authentication

FR-SEC-05 requires authenticated service calls. Falcone authenticates every
report against `ctr-platform-service-identity` and derives the caller identity
only from that verified credential. A report whose identity Falcone cannot verify
is an unverified identity and is refused under [Authorization](#authorization).
No report field establishes or substitutes for identity.

## Authorization

Falcone takes the allowed namespaces only from the verified identity plus
`ctr-namespace-bindings`, never from report fields. `poolRef.namespace` is a claim
to be checked against that set, not a source of scope. Bindings resolve per
`namespaceMode` (ADR 0011): in `shared` mode every tenant's pools live in one
namespace, so a namespace match alone does not establish tenant ownership; Falcone
also checks the named pool against `ctr-resource-ownership`.

Authorization runs before ordering. A refused report never reaches the ordering
rules and never changes stored state.

| Rule | Condition | Outcome | State |
| --- | --- | --- | --- |
| `unverified_identity` | The report's identity cannot be verified under ctr-platform-service-identity | refused | unchanged |

## Generation semantics

- `observedGeneration` is the Falcone desired pool configuration and routing
  generation the controller last applied. Readiness changes do not alter it.
- `reporterEpoch` is the fencing token of the controller's namespaced Lease
  (FR-STO-06). It must be monotonic across leader changes. If the controller
  cannot read a monotonic fencing token from its Lease, `contractVersion` must be
  bumped before it reports. The controller reads the epoch recorded on the Lease,
  writes the epoch plus one with optimistic concurrency (resourceVersion) for a new
  term, and never derives it from a wall clock.
- `reporterLineage` (revision 1.2) is the `metadata.uid` of that Lease, a
  non-empty string without whitespace. An epoch is monotonic only within one
  Lease, so deleting and recreating the Lease starts a new lineage whose epochs
  restart and can equal numbers the receiver already accepted. The lineage is
  what tells the two apart. Every fenced write and report carries it with the
  epoch and sequence. The controller never infers, learns or persists a
  lineage or epoch from a receiver refusal.
- A changed `readAt` or `resourceVersion` is a different payload and requires
  a new `observationSeq`. Reusing an equal key for that payload is refused as
  a conflict. Encoding the same observation must be deterministic.
  The encoder canonicalizes conditions, revisions and verifiedAbsence by their
  complete encoded entries and ownedObjects by kind, UID and resourceVersion,
  so input list order does not change the payload. It preserves caller inputs.
- `observationSeq` rises with each new observation within an epoch. The design has
  no such counter, so it reuses the Lease and fencing patterns: the controller
  keeps it in memory, restarts it when `reporterEpoch` rises, and never reuses a
  value within an epoch.

## Ordering cases

Falcone compares a report's key `(observedGeneration, reporterEpoch,
observationSeq)` with the stored key lexicographically: generation first, then
epoch, then sequence, within one lineage. A report with `observedGeneration` above Falcone's current
desired generation is refused before any key comparison. A report whose
`reporterEpoch` is below the stored epoch comes from a fenced reporter: it is
refused, state unchanged, whatever its generation or sequence. The receiver
records the lineage with the epoch and sequence it stores, and refuses any
revision 1.2 report whose `reporterLineage` differs from the recorded one,
whatever its epoch, generation or sequence, with the state unchanged. This check
runs before the epoch guard and the key comparison, so a recreated Lease cannot
publish at an equal, lower or higher epoch until an operator resets the receiver's
recorded lineage and epoch for this singleton. The reset names the lineage to bind,
the `metadata.uid` of the current Lease: the receiver stores that uid with epoch 0
and sequence 0, and never adopts the lineage of whichever report arrives next. A
report delayed from the deleted lineage that arrives right after the reset therefore
differs from the recorded lineage and is refused. Only a singleton that has never
accepted a report has no recorded lineage, and the receiver records the first one's.
Ordering within one lineage is unchanged. Otherwise:

- Lower key: refused, state unchanged.
- Equal key and identical payload: a replay, accepted as a no-op.
- Equal key and different payload: refused as a conflict.
- Higher key: applied, even if `readyReplicas` falls.

The epoch guard keeps a reporter from an older Lease epoch from overwriting the
observation of the newer leader, even when it has applied a higher generation
(DEP-03 ac1: stale updates cannot overwrite newer state). Rows cover the cases
below; the conformance test checks every outcome against a reference comparator.

Outcomes are `applied`, `replay_noop`, `refused` and `conflict_refused`. Keys are
`(generation,epoch,seq)`.

| Case | Stored key | Report key | Payload | Desired generation | Outcome | Notes |
| --- | --- | --- | --- | --- | --- | --- |
| G1 | (5,2,10) | (4,2,11) | different | 5 | refused | lower generation |
| G2 | (5,2,10) | (5,2,10) | same | 5 | replay_noop | equal key, same payload |
| G3 | (5,2,10) | (5,2,10) | different | 5 | conflict_refused | equal key, different payload |
| G4 | (5,2,10) | (5,2,11) | different | 5 | replay_noop | readyReplicas 0 to 1 |
| G5 | (5,2,11) | (5,2,12) | different | 5 | applied | readyReplicas 1 to 0 |
| G6 | (5,2,10) | (5,2,9) | different | 5 | refused | same epoch, lower seq |
| G7 | (5,2,10) | (5,3,1) | different | 5 | applied | higher epoch, lower seq |
| G8 | (5,2,10) | (7,2,1) | different | 6 | refused | generation above desired |
| G9 | (5,2,10) | (6,1,1) | different | 6 | refused | lower epoch, higher generation |

Lineage cases extend the table above across a Lease deletion. Keys are
`(generation,epoch,seq)` and both lineages are literals. A report with a recorded
lineage and key is judged first by lineage, then by the rows above.

| Lineage case | Stored lineage | Report lineage | Stored key | Report key | Outcome | Notes |
| --- | --- | --- | --- | --- | --- | --- |
| L1 | A | A | (5,2,10) | (5,2,11) | applied | same lineage, ordering unchanged |
| L2 | A | B | (5,2,10) | (5,2,11) | refused | different lineage, higher seq |
| L3 | A | B | (5,2,10) | (5,2,10) | refused | different lineage, equal key, never a replay or conflict |
| L4 | A | B | (5,2,10) | (5,3,1) | refused | different lineage, higher epoch |
| L5 | A | B | (5,2,10) | (5,1,1) | refused | different lineage, lower epoch |
| L6 | A | B | (5,2,10) | (6,9,1) | refused | different lineage, higher generation and epoch |
| L7 | A | A | (5,2,10) | (5,1,99) | refused | same lineage, fenced epoch |

## Repair conditions

These conditions are reported per `ctr-resource-ownership`. Each keeps the
resource and blocks completion. They are operator-visible only; a tenant sees only
`deletion_pending`. Completion after cleanup requires `absenceVerified=true`
together with revision 1.1 and verifiedAbsence evidence, and the controller sets
it only after it has verified that the resources it owned are absent.

| Condition | Effect |
| --- | --- |
| `UIDMismatch` | Keep the resource and block completion; status stays deletion_pending |
| `ResourceVersionMismatch` | Keep the resource and block completion; status stays deletion_pending |
| `OwnershipMismatch` | Keep the resource and block completion; status stays deletion_pending |
| `MissingPreconditions` | Keep the resource and block completion; status stays deletion_pending |
| `TerminationPending` | Keep the resource and block completion while descendants remain; status stays deletion_pending |

## Status mapping

Each row maps to one of the five statuses of `ctr-runtime-status-model`. Status
is not a routing input: traffic gating uses readiness only.

| Status | Derivation |
| --- | --- |
| `disabled` | Falcone disables or suspends the function or tenant; a report never sets it |
| `unavailable` | The pool has no ready capacity it should have, or a failure condition below is present |
| `deploying` | observedGeneration is below Falcone's desired generation, or readyReplicas is below desiredReplicas with no failure condition |
| `active` | observedGeneration equals the desired generation and readyReplicas equals desiredReplicas, including a pool scaled to zero |
| `deletion_pending` | Falcone has recorded deletion; stays pending until contractRevision is 1.1 or later, absenceVerified=true and nonempty verifiedAbsence evidence are present together |

### OCI conditions

Conditions of an OCI pool (`runtimeMode` oci). The condition is operator-visible.
The tenant sees the status and the tenant reason, which contains no registry path,
pod name or node name and matches `^[a-z][a-z0-9_]*$`.
The schema and validator accept these condition types on Wasm reports as well;
they validate the shared condition vocabulary, not its runtime applicability.
Producers must emit these conditions only for OCI pools, and consumers must apply
the following mapping only when runtimeMode is oci.

| Condition | Visibility | Status | Tenant reason |
| --- | --- | --- | --- |
| `ImagePullFailed` | operator | unavailable | registry.example.com/team/app pull failed |
| `ContainerStartFailed` | operator | unavailable | container_start_failed |
| `ContractProbeFailed` | operator | unavailable | contract_probe_failed |
| `Dematerialized` | operator | active | scaled_to_zero |

An idle OCI revision is dematerialized and activates on demand, so it stays
`active`. Revocation of OCI authorization is reported by Falcone as
`oci_not_authorized`, not through this report.
