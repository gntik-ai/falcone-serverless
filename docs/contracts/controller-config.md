# Controller configuration and prerequisites

Contract: ctr-controller-config
Version: 1

This contract defines configuration inputs and permission ceilings, not a runtime
parser or preflight implementation. Breaking changes require version 2 and a
conformance-test update in the same change. No config key widens the Role.

## Configuration

| Key | Default or requirement |
| --- | --- |
| `namespaceMode` | `preprovisioned` (default); `create` and `shared` are defined by ADR 0011 but not activated or qualified by this contract. |
| `watchedPoolNamespaces` | Required explicit namespace names, restricted to verified pool bindings; no all-namespace watch. |
| `watchedOCINamespaces` | Explicit namespace names for enabled OCI bindings; no all-namespace watch. Required before OCI is enabled. |
| `namespaceBindingsPath` | `/etc/falcone/namespace-bindings.json`; required operator-owned file, format owned by TEN-01 (`ctr-namespace-bindings`). |
| `controllerNamespace` | Required existing namespace for the controller's two Leases and service identity. |
| `leaderElectionLeaseName` | `falcone-pool-controller-leader` (fixed by ctr-resource-ownership). |
| `scalerLeaseName` | `falcone-pool-controller-scaler` (fixed by ctr-resource-ownership). |
| `leaseDurationSeconds` | `15`; positive duration, greater than renewDeadlineSeconds. |
| `renewDeadlineSeconds` | `10`; positive duration, greater than retryPeriodSeconds. |
| `retryPeriodSeconds` | `2`; positive duration. Loss of a duty's Lease stops that duty's writes; no unfenced singleton work. |
| `falconeDesiredStateEndpoint` | Required operator-supplied HTTPS endpoint for authoritative desired state. |
| `falconeStatusEndpoint` | Required operator-supplied HTTPS endpoint for ctr-pool-status-report. |
| `serviceIdentitySecretName` | `falcone-controller-service-identity` in controllerNamespace; existing service-identity Secret reference for ctr-platform-service-identity, by name only. |
| `platformRegistryPullSecretName` | `falcone-platform-registry-pull` in every bound OCI namespace; existing platform intake-mirror pull Secret reference, by name only. |
| `metricsPort` | `9090`; metrics listener. |
| `probePort` | `8081`; readiness and liveness listener, distinct from metricsPort. |

The controller never creates namespaces or namespaced RBAC. In `preprovisioned`
mode the administrator supplies dedicated namespaces and permissions. Only the
installer may create dedicated namespaces and namespaced RBAC in `create` mode,
after preflight proves the deploying identity's authority. Shared mode resolves
bindings to the release namespace and requires tenant-scoped enforced
NetworkPolicies, distinct tenant service accounts and credential/RLS checks.
Unknown or unauthorized bindings fail closed before writes. Runtime placement is
independent of `testNamespaceMode`; this contract activates neither create nor
shared and grants no installer authority by itself.

## Prerequisites

Exactly these prerequisites must already be available and verified before the
controller creates or adopts anything. The controller reports each missing
prerequisite, stays not ready, and nothing is created or adopted. Never silently
disable isolation, provision a missing facility or adopt an existing object.

| Prerequisite | Requirement |
| --- | --- |
| namespaces | Preprovisioned namespaces by default, resolved from verified bindings according to namespaceMode; controllerNamespace also exists. Only the installer may provision dedicated namespaces in create mode after permission preflight; this contract does not activate create or shared. |
| gateway | Existing ingress/TLS gateway, plus the enforced OCI egress gateway when OCI is enabled. |
| storage | Existing PostgreSQL and object storage with authorized bindings. |
| broker | Existing Kafka broker and authorized connectivity. |
| RBAC | Pregranted namespaced permissions exactly bounded by the RBAC table below. |
| network-policy enforcement | Verified ingress and egress NetworkPolicy enforcement; shared placement requires tenant-scoped selectors, and missing enforcement blocks OCI. |
| existing SCC | Existing operator-provided SCC by reference only; never a credential value or permission to alter admission. |

## RBAC

One fixed table defines the complete namespaced controller Role. Write rows also
include the reads needed for safe ownership checks. No Jobs or direct Pod access.
Secret get is restricted with resourceNames to the two configured Secret names
in their respective namespaces; no Secret list or watch. NetworkPolicy is read
only; provisioning and enforcement belong to the operator/installer.

| Access | Kind | Verbs | Scope |
| --- | --- | --- | --- |
| write | Deployment | get, list, watch, create, update, patch, delete | Bound pool and OCI namespaces |
| write | Service | get, list, watch, create, update, patch, delete | Bound pool and OCI namespaces |
| write | Lease | get, list, watch, create, update, patch, delete | controllerNamespace, the two fixed Lease names for mutations |
| read | ConfigMap | get, list, watch | Authorized configuration in explicitly watched namespaces |
| read | Secret | get | Named Secrets only via resourceNames, never credential values in reports or logs |
| read | NetworkPolicy | get, list, watch | Bound namespaces, prerequisite inspection only |

No config key widens the Role. OCI adds no verb or kind. Permissions are granted
externally, never self-granted; no fallback widens access on a denied read/write.

## OCI

`watchedOCINamespaces` resolves only authorized bindings under namespaceMode.
The `platformRegistryPullSecretName` must already exist in each resolved OCI
namespace; kubelets pull only from the platform intake mirror. The controller
references its name, never copies, creates or updates Secret values. Shared OCI
also requires the per-tenant superadmin enablement from ADR 0011 and verified
tenant-scoped network enforcement, in addition to project OCI authorization.
The Deployment and headless Service from ctr-resource-ownership use exactly the
RBAC table above; OCI adds no verb or kind.

## Forbidden

CustomResourceDefinition (CRD), ClusterRole, ClusterRoleBinding and custom
RuntimeClass are forbidden. Creating or modifying any SCC
(SecurityContextConstraints) is forbidden. The existing SCC is a prerequisite by
reference only. No Namespace, RBAC, Pod, Job or Secret writes, elevated pods,
host access or admission changes are authorized in any namespaceMode.
