# Controller resource ownership

Contract: ctr-resource-ownership
Version: 1

This contract fixes names and ownership for the namespaced pool controller.
Breaking changes require version 2 and a conformance-test update in the same
change. Falcone remains the desired-state authority. This document defines rules;
it implements no reconciliation, scaling or cleanup logic.

## Naming

Bindings from `ctr-namespace-bindings` select the authorized namespace according
to `namespaceMode` (ADR 0011); a namespace name alone proves no tenant ownership.
For Wasm, one Deployment and one Service represent each tenant/profile pool.
Both use `falcone-pool-<pool-id>`. Compute pool-id as the first 40 lowercase hex
characters of SHA-256 over the UTF-8 tenant ID and profile ID, each prefixed with
its unsigned 32-bit big-endian byte length. No user-controlled spelling is used
in resource names. The full identity is retained in ownership annotations;
a truncated-hash collision is an OwnershipMismatch, never an adoption.

The controller namespace holds the leader-election Lease
`falcone-pool-controller-leader` and the scaler Lease
`falcone-pool-controller-scaler`. Each singleton duty has one holder across HA
replicas; holding one Lease does not authorize the other duty. Lease names are
fixed by this version, not arbitrary extra resource names.

## Ownership

Pool Deployments and Services carry the following metadata. Their pod templates
carry the pool labels used by Service selectors.

| Metadata | Key | Value |
| --- | --- | --- |
| Label | `falcone.ai/owner` | `pool-controller` |
| Label | `falcone.ai/tenant` | SHA-256 of the length-prefixed tenant ID, first 40 lowercase hex characters |
| Label | `falcone.ai/pool` | pool-id (revision pool-id for OCI) |
| Label | `falcone.ai/runtime-mode` | `wasm` or `oci` |
| Annotation | `falcone.ai/owner` | Full tenant/profile identity (and revision for OCI), JSON array of strings in that order |
| Annotation | `falcone.ai/desired-state-generation` | Falcone desired-state generation, decimal integer |
| Annotation | `falcone.ai/ownership-version` | `1` |

Singleton Leases use the
`falcone.ai/owner=pool-controller` label and `falcone.ai/ownership-version=1`
annotation; their owner annotation identifies the controller namespace and duty
as a JSON array, rather than a tenant pool. Lease renewals use Kubernetes
optimistic concurrency and never take over an unverified existing Lease.

The server-side apply field manager is `falcone-pool-controller`. The exclusive
replica-count writer uses field manager `falcone-pool-scaler` while holding the
scaler Lease. Desired-state apply omits `spec.replicas`; it must not reset the
scaler's count or force ownership of another manager's fields.

Labels are not a security boundary or API authorization. Shared mode additionally
relies on enforced tenant-scoped NetworkPolicies, distinct service accounts,
verified bindings, scoped credential checks and SQL/RLS checks (ADR 0011).
This contract does not activate or qualify create or shared mode.

On every pool reconcile and delete, verify the bound tenant/profile/revision owner,
ownership format version, observed UID and resourceVersion against the recorded
owned object before any mutation. Match annotations as well as labels; a matching
name alone never authorizes adoption. An unrecorded existing object is not adopted.
Creation is allowed only after verified absence in the authorized binding.
Use UID and resourceVersion preconditions for deletion and optimistic concurrency
for writes. A conflict requires a fresh read and ownership verification, never a
blind retry, forced apply or replacement. Record the new resourceVersion after a
successful authorized write; a newly observed version is not automatically trusted.

## Repair conditions

These operator-visible conditions are consumed by `ctr-pool-status-report`.
On mismatch, keep the resource and block completion, including deletion and
`absenceVerified=true`, until ownership is safely resolved. Do not overwrite,
adopt or delete a mismatching resource.

| Condition | Trigger and effect |
| --- | --- |
| `UIDMismatch` | Observed UID differs from recorded UID: keep the resource and block completion. |
| `ResourceVersionMismatch` | Observed resourceVersion differs from the expected version, including a precondition conflict: keep the resource and block completion. |
| `OwnershipMismatch` | Bound tenant/profile/revision, owner metadata or ownership format version differs or is missing: keep the resource and block completion. |

## OCI

Each materialized OCI revision has one Deployment and one headless Service
(`clusterIP: None`) in the tenant's bound OCI namespace. Both use
`falcone-oci-<revision-pool-id>`, where revision-pool-id is the first 40 lowercase
hex characters of SHA-256 over length-prefixed UTF-8 tenant ID, profile ID and
immutable revision ID, using the same byte-length encoding as Naming. Names stay
unique across tenants within one namespace; collisions fail closed under the
ownership rules above. A dematerialized revision has no serving pool objects.

Both resources and the Deployment's pod template carry `falcone.ai/runtime-mode`
with value `oci` and `falcone.ai/revision` with the first 40 lowercase hex
characters of SHA-256 over the length-prefixed revision ID. The full immutable
revision ID remains in the owner annotation. Service selectors include tenant,
pool, runtime mode and revision labels, so they never select another revision.
Wasm Service selectors likewise include tenant, pool and runtime mode labels;
Wasm pod templates carry those same labels.

OCI uses the same ownership checks, field managers and permitted kinds and verbs.
Restricted pods and ADR 0010's authorization, shim, credential and egress rules
remain mandatory. Shared OCI additionally needs the tenant's explicit superadmin
enablement and verified tenant-scoped NetworkPolicy enforcement (ADR 0011).

## Forbidden

Never create or manage a CustomResourceDefinition (CRD), ClusterRole,
ClusterRoleBinding or custom RuntimeClass. Creating or modifying any SCC
(SecurityContextConstraints) is forbidden; only an existing operator-provided
SCC may be referenced as a prerequisite. No controller Namespace, RBAC, Pod,
Job or Secret writes, host access, elevated privileges or tenant native fallback
are authorized. OCI adds no kind or verb.
