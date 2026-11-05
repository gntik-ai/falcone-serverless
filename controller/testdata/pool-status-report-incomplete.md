<!-- Negative fixture for controller/pool_status_report_contract_test.go: a copy of the contract with five defects: no observationSeq field, case G4 marked replay_noop, no unbound_namespace refusal rule, tenant-visible poolRef.namespace, and a tenant-visible ImagePullFailed reason that carries a registry path. -->
# Pool status report

Contract: ctr-pool-status-report
contractVersion: 1

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
Report cadence, heartbeat freshness and scaling metrics are out of scope. Any
change to this contract bumps `contractVersion` and updates the conformance
test and its reference comparator in the same change
(`controller/pool_status_report_contract_test.go`).

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

Notes:

- `Required` is `yes` for every report. `oci` means required when
  `runtimeMode` is `oci` and absent otherwise.
- `revision` is the Falcone revision ID of the single revision an OCI pool
  serves. For `oci`, `revisions` holds exactly the entry named by `revision`.
  `materialized` is true while the pool's Deployment and Service exist and false
  after dematerialization. It is operator-visible.
- `readyReplicas`, `readyCapacity` and `revisions[].ready` are readiness. They
  can change without `observedGeneration` changing.
- `conditions[]` has no free-text message field. Condition types are listed in
  [Repair conditions](#repair-conditions) and [Status mapping](#status-mapping).
  Tenant-visible text comes only from the mapping tables, never from a report field.

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
  bumped before it reports.
- `observationSeq` rises with each new observation within an epoch. The design has
  no such counter, so it reuses the Lease and fencing patterns: the controller
  keeps it in memory, restarts it when `reporterEpoch` rises, and never reuses a
  value within an epoch.

## Ordering cases

Falcone compares a report's key `(observedGeneration, reporterEpoch,
observationSeq)` with the stored key lexicographically: generation first, then
epoch, then sequence. A report with `observedGeneration` above Falcone's current
desired generation is refused before any key comparison. Otherwise:

- Lower key: refused, state unchanged.
- Equal key and identical payload: a replay, accepted as a no-op.
- Equal key and different payload: refused as a conflict.
- Higher key: applied, even if `readyReplicas` falls.

Because generation is compared before epoch, a higher generation from a reporter
with a lower epoch is applied. Rows cover the cases below; the conformance test
checks every outcome against a reference comparator.

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

## Repair conditions

These conditions are reported per `ctr-resource-ownership`. Each keeps the
resource and blocks completion. They are operator-visible only; a tenant sees only
`deletion_pending`. `absenceVerified=true` is the only completion signal after
cleanup, and the controller sets it only after it has verified that the resources
it owned are absent.

| Condition | Effect |
| --- | --- |
| `UIDMismatch` | Keep the resource and block completion; status stays deletion_pending |
| `ResourceVersionMismatch` | Keep the resource and block completion; status stays deletion_pending |
| `OwnershipMismatch` | Keep the resource and block completion; status stays deletion_pending |

## Status mapping

Each row maps to one of the five statuses of `ctr-runtime-status-model`. Status
is not a routing input: traffic gating uses readiness only.

| Status | Derivation |
| --- | --- |
| `disabled` | Falcone disables or suspends the function or tenant; a report never sets it |
| `unavailable` | The pool has no ready capacity it should have, or a failure condition below is present |
| `deploying` | observedGeneration is below Falcone's desired generation, or readyReplicas is below desiredReplicas with no failure condition |
| `active` | observedGeneration equals the desired generation and readyReplicas equals desiredReplicas, including a pool scaled to zero |
| `deletion_pending` | Falcone has recorded deletion and absenceVerified is not yet true |

### OCI conditions

Conditions of an OCI pool (`runtimeMode` oci). The condition is operator-visible.
The tenant sees the status and the tenant reason, which contains no registry path,
pod name or node name and matches `^[a-z][a-z0-9_]*$`.

| Condition | Visibility | Status | Tenant reason |
| --- | --- | --- | --- |
| `ImagePullFailed` | operator | unavailable | registry.example.com/team/app pull failed |
| `ContainerStartFailed` | operator | unavailable | container_start_failed |
| `ContractProbeFailed` | operator | unavailable | contract_probe_failed |
| `Dematerialized` | operator | active | scaled_to_zero |

An idle OCI revision is dematerialized and activates on demand, so it stays
`active`. Revocation of OCI authorization is reported by Falcone as
`oci_not_authorized`, not through this report.
