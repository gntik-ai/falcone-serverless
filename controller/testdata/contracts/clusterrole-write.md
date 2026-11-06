# Controller config fixture

Contract: ctr-controller-config
Version: 1

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

## Prerequisites

| Prerequisite | Requirement |
| --- | --- |
| namespaces | Preprovisioned namespaces by default, resolved from verified bindings according to namespaceMode; controllerNamespace also exists. Only the installer may provision dedicated namespaces in create mode after permission preflight; this contract does not activate create or shared. |
| gateway | Existing ingress/TLS gateway, plus the enforced OCI egress gateway when OCI is enabled. |
| storage | Existing PostgreSQL and object storage with authorized bindings. |
| broker | Existing Kafka broker and authorized connectivity. |
| RBAC | Pregranted namespaced permissions exactly bounded by the RBAC table below. |
| network-policy enforcement | Verified ingress and egress NetworkPolicy enforcement; shared placement requires tenant-scoped selectors, and missing enforcement blocks OCI. |
| existing SCC | Existing SCC restricted-v2 by reference only. |

## RBAC

No config key widens the Role. OCI adds no verb or kind.

| Access | Kind | Verbs | Scope |
| --- | --- | --- | --- |
| write | Deployment | get, list, watch, create, update, patch, delete | Bound pool and OCI namespaces |
| write | Service | get, list, watch, create, update, patch, delete | Bound pool and OCI namespaces |
| write | Lease | get, list, watch, create, update, patch, delete | controllerNamespace, create is unrestricted by name in controllerNamespace; resourceNames restricts update, patch and delete to the two fixed Lease names |
| read | ConfigMap | get, list, watch | Authorized configuration in explicitly watched namespaces |
| read | Secret | get | Named Secrets only via resourceNames, never credential values in reports or logs |
| read | NetworkPolicy | get, list, watch | Bound namespaces, prerequisite inspection only |
| write | ClusterRole | create | extra |

## OCI

OCI adds no verb or kind.

## Forbidden

CustomResourceDefinition (CRD), ClusterRole, ClusterRoleBinding and custom RuntimeClass.
Creating or modifying any SCC (SecurityContextConstraints) is forbidden.
