# Install prerequisites and the controller preflight

Contracts: ctr-controller-config v1 (`docs/contracts/controller-config.md`) and the
local report `pool-preflight/v1` (`docs/contracts/pool-preflight/v1`). This is
OPS-01. The report is not `ctr-pool-status-report`, the per-pool report the
controller sends to Falcone.

At start-up the controller runs a read-only preflight before it does any work. It
asks the Kubernetes API, with SelfSubjectAccessReviews only, whether its own
identity holds the verbs below, and it reports whether the values file declares
every prerequisite. It prints one `pool-preflight/v1` JSON report to stdout. When
the result is `failed` it exits 1 and starts nothing; when it is `passed` it
continues with Lease leader election until SIGTERM/SIGINT. The preflight creates,
updates, adopts and deletes nothing. Election uses only get/create/update on the
fixed leader Lease in the release namespace; pool duties remain no-ops until DEP-03.
See [degraded behavior](degraded-behavior.md) for timing and shutdown semantics.

The chart must inject `POD_NAMESPACE` using the downward API `fieldRef:
metadata.namespace`. A missing, invalid or mismatched value fails with configError
and exit 1 before access reviews or Lease requests. This is a deployment dependency
for gntik-ai/falcone-charts; the source repository does not render the chart.

## Values the controller reads

The controller reads the JSON values file named by `FALCONE_SERVERLESS_VALUES`,
default `/etc/falcone-serverless/values.json`. It reads only the fields below and
ignores every other key. This is the consumer side of `ctr-serverless-chart-values`;
the chart owner in falcone-charts must render these exact field names. A name that
differs fails closed as a missing prerequisite.

| Field | Meaning |
| --- | --- |
| `watchedPoolNamespaces` | Required explicit list of DNS-1123 namespace names, no duplicates, no `*`. |
| `controllerNamespace` | Required DNS-1123 namespace name holding the two Leases; must equal `POD_NAMESPACE`. |
| `leaderElectionLeaseName` | Absent or `falcone-pool-controller-leader`; every other value fails closed. |
| `leaseDurationSeconds` | Integer seconds, default 15, maximum 300. |
| `renewDeadlineSeconds` | Integer seconds, default 10, strictly less than lease duration. |
| `retryPeriodSeconds` | Integer seconds, default 2, strictly less than renew deadline and greater than zero. |
| `namespaceMode` | Absent means `preprovisioned`. `create`, `shared`, a blank value and any other value fail closed before any request: neither mode is implemented. |
| `prerequisites.gatewayRoute.name` | Name of the existing gateway route. |
| `prerequisites.storage.name` | Name of the existing storage binding. |
| `prerequisites.broker.name` | Name of the existing broker binding. |
| `prerequisites.rbac.controllerRoleBinding` | Name of the pregranted controller RoleBinding. |
| `prerequisites.networkPolicyEnforcement.name` | Name of the facility that enforces NetworkPolicy. |
| `prerequisites.existingSCC.name` | Name of the existing operator-provided SCC, by reference only. |

The access reviews authenticate with the in-cluster service account token and CA and
reach the API through `KUBERNETES_SERVICE_HOST` and `KUBERNETES_SERVICE_PORT`. Each
request has a 10 second timeout, redirects are not followed and no proxy is used.
The access reviews work through the cluster's default binding for
SelfSubjectAccessReview, not the controller Role; a cluster that removes that
binding reports `error` and fails closed.

## Prerequisite kinds

The controller reports each kind as `declared` (the values file names it) or
`missing` (absent or blank). A `declared` state is not a verification: the
controller does not check that a gateway, storage, broker, enforcement facility or
SCC exists, and no report field claims it. A `missing` kind makes the result
`failed`. Named Secrets stay with DEP-03.

| Kind | Values field | Provider | Verifier |
| --- | --- | --- | --- |
| `namespace` | `watchedPoolNamespaces`, `controllerNamespace` (one entry per name) | Administrator, who provisions the dedicated namespaces | Administrator at install; the controller only checks its verbs in them |
| `gateway` | `prerequisites.gatewayRoute.name` | Platform operator (existing ingress and TLS gateway) | Operator at install; not verified by the controller |
| `storage` | `prerequisites.storage.name` | Platform operator (existing PostgreSQL and object storage) | Operator at install; not verified by the controller |
| `broker` | `prerequisites.broker.name` | Platform operator (existing Kafka broker) | Operator at install; not verified by the controller |
| `rbac` | `prerequisites.rbac.controllerRoleBinding` | Administrator, who pregrants the namespaced Role and RoleBinding | The access checks below prove the verbs are allowed, not that the grant is exactly bounded |
| `network-policy-enforcement` | `prerequisites.networkPolicyEnforcement.name` | Cluster operator (CNI that enforces NetworkPolicy) | Operator at install; not verified by the controller |
| `existing-scc` | `prerequisites.existingSCC.name` | Cluster operator (existing SCC, by reference only) | Operator at install; not verified by the controller |

## Access checks

One exported table in `controller/internal/preflight` drives every check: 14 per
pool namespace plus 10 in the controllerNamespace, 14N+10 in all (38 for two pool
namespaces). Every check is a SelfSubjectAccessReview with a namespace; an empty
namespace would ask about all namespaces, so none is sent. Each check reports
`allowed`, `denied` or `error`, and the preflight runs all of them without stopping
at the first failure. Leases are checked only in the controllerNamespace; the
Deployment and Service checks target the controllerNamespace only when it is also a
pool namespace.

| Scope | Group | Resource | Verbs | Names |
| --- | --- | --- | --- | --- |
| each pool namespace | `apps` | `deployments` | `get`, `list`, `watch`, `create`, `update`, `patch`, `delete` | any |
| each pool namespace | (core) | `services` | `get`, `list`, `watch`, `create`, `update`, `patch`, `delete` | any |
| controllerNamespace | `coordination.k8s.io` | `leases` | `get`, `list`, `watch`, `create` | any |
| controllerNamespace | `coordination.k8s.io` | `leases` | `update`, `patch`, `delete` | `falcone-pool-controller-leader`, `falcone-pool-controller-scaler` |

## What `passed` means

`passed` means only that the checks in this table passed and that every prerequisite
kind is declared. It is partial coverage:

- An `allowed` answer does not show that a grant is exactly bounded; an over-broad
  grant also answers `allowed`.
- Namespace bindings and NetworkPolicy enforcement are not proven.
- The ConfigMap get, list and watch, Secret get and NetworkPolicy get, list and
  watch reads from the RBAC table of `controller-config.md` are not checked here.
  They move to the follow-up task named "OPS-01 preflight read checks". Named
  Secrets stay with DEP-03.

`failed` covers a denied or `error` check, a missing prerequisite and any
configuration error. A configuration error (unreadable or invalid values file, an
empty or invalid namespace list, an unsupported `namespaceMode`, an unreadable or
empty token, or an unset API host) sends no request at all and is reported in
`preflight.configError`. When no values could be read, every kind is reported
`missing` with an empty name and `checks` is empty. The report never contains the
token, an API response body or values-file content beyond namespace and
prerequisite names and the `namespaceMode` value.

The image therefore exits 1 outside a configured cluster. A pod that cannot pass the
preflight restarts and stays unavailable, which is the intended fail-closed state.
