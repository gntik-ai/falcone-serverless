# WAS-01 Kubernetes and namespaced-permission evidence

This report records ac3 observations and checks the ac1 command plan. It never produces
WAS-01 go approval: ac2, real OpenShift, selected pins and guest evidence are absent.
It is not connected to the worker runtime and does not change versions.toml.

The targets are `k8s-1.37`, `k8s-1.36` and `openshift-4.20`. An exact, validated
`/version` gitVersion is retained, including a suffix such as `v1.36.4+k3s1`.
A passed `kubernetes-version` means **recorded**, not supported or qualified.
For example, `v1.35.0`, `v1.370.0` and `v2.37.0` are recorded but fill no target cell.
The shared cluster is k3s, a stand-in only; it supplies no OpenShift evidence.
Each matching Kubernetes cell is failed if any ac3 item failed, passed only when
all ac3 items passed, and otherwise unverified. Unmatched cells stay unverified.
Without a recorded version, any ac3 failure marks both Kubernetes cells failed;
the OpenShift cell stays unverified.

## Item inventory

- `plan-namespaced-only`
- `kubernetes-version`
- `can-get-deployments.apps`, `can-create-deployments.apps`, `can-update-deployments.apps`, `can-delete-deployments.apps`
- `can-get-services`, `can-create-services`, `can-update-services`, `can-delete-services`
- `can-get-leases.coordination.k8s.io`, `can-create-leases.coordination.k8s.io`, `can-update-leases.coordination.k8s.io`, `can-delete-leases.coordination.k8s.io`
- `openshift-4.20`
- `pin-engine`, `pin-architecture`, `pin-wasi`, `pin-compiler`
- `guest-install`, `guest-execute`, `guest-interrupt`

Items start unverified. Passing requires validated evidence. Unknown IDs, duplicate
or malformed lines and inconsistent summaries/cells are rejected on report parsing.
Reports use a version header, tab-separated item lines (ID, verdict, reason, evidence),
derived cell lines and a summary listing every item or target that has not passed.
Only a version, `yes`, or `allow-listed-plan` can be evidence; diagnostics are never evidence.

## Fixed reason codes

| Code | Meaning |
| --- | --- |
| `not-probed` | No cluster observation was made. |
| `recorded` | A valid exact gitVersion was recorded. |
| `allowed` | can-i returned yes with exit zero. |
| `denied` | can-i returned `no` or `no - <reason>` with exit zero or one; the reason text is discarded. |
| `probe-error` | Execution, timeout, oversized output or an unrecognized response. |
| `forbidden` | A line-anchored kubectl Forbidden diagnostic on stderr. |
| `invalid-version` | Missing or malformed gitVersion; nothing is recorded. |
| `openshift-not-available` | No real OpenShift cluster evidence. |
| `no-pin-selected` | Engine, architecture, WASI and compiler pins are not selected. |
| `not-collected` | Guest install, execute and interrupt evidence is not collected. |
| `plan-safe` | Every command matches the exact allow-list. |
| `plan-rejected` | The plan violates the allow-list. |

No arbitrary reason text is accepted. Stderr is drained with bounded memory (8192 bytes
retained per stream), classified and discarded. Overflow fails closed as probe-error,
even if a Forbidden marker is beyond the bound. The call deadline kills the child
process group; output never extends the deadline. Stderr cannot supply passing evidence.

## Offline and operator-only modes

`was01_ac3_report` in `worker/tests/was01_cluster.rs` writes a report beneath
`CARGO_TARGET_TMPDIR`. Leave either `FS_WAS01_CLUSTER` or `FS_TEST_CLUSTER` unset
(the kill switches). Only **both** equal to `1` enable this report's cluster mode.
Offline mode makes zero runner calls and keeps ac3 unverified/not-probed.
Reports are test artifacts and must never be committed.

Cluster contact is for the operator only, after the R3 second review and explicit
operator approval. Probe as the deployment identity: permissions describe only the
identity in the supplied kubeconfig, not a tenant or a different service account.
`KUBECONFIG` is required and uses the existing explicit list/merge validator. Every
kubectl invocation uses `Kubeconfigs::kubectl_flags`; there is no ambient config fallback.
An invalid list fails before any runner call. The process runner has a 10-second
timeout for every call, including namespace cleanup with --wait.
It uses no image, pod, guest or existing WAS-01 ac1/ac2 interface.
The existing separate ac1/ac2 cluster test retains its own image requirement.

`FS_TEST_NAMESPACE_MODE` must be unset or `create`. `shared` is refused before
any call, even if `FS_TEST_NAMESPACE` names a namespace. This report does not implement
shared mode and never falls back to it if creation is denied. Its error names
`FS_TEST_NAMESPACE_MODE`. The generated name is `fs-test-was01-<run>`, a DNS label
of at most 63 characters, using the existing Namespace type and namespace_name.

The exact command allow-list is: get --raw /version; create namespace <this-run>;
delete namespace <this-run> --wait; and auth can-i <get|create|update|delete>
<deployments.apps|services|leases.coordination.k8s.io> -n <this-run>.
Any extra argument or different form is refused. No resource is created inside the
namespace, no permissions are granted, and no cluster-scoped resource is read.
This report is an exception to the root README's test_namespace(suite) preflight:
that preflight's unnamespaced command is outside this report's allow-list.
Its exact create command also omits the restricted-enforcement namespace label;
the report creates no pods or namespaced workloads.
The only cluster-scoped write is this harness identity creating its own namespace.

Ownership begins only when create succeeds. Creation failures, including AlreadyExists,
never trigger deletion. After success, cleanup deletes exactly that namespace with
--wait, including on later failure or panic; failed cleanup returns an error.
Failures identify the namespace and report path. A cleanup failure during panic
is logged with those same details so the operator can locate an orphan.
SIGKILL, machine failure, or an ambiguous create timeout can leave an orphan. An operator
must inspect and manually clean up orphaned namespaces; later runs never delete them.

## Dependency guard

The model is std-only. The guard pins dependency tables in the workspace and worker
manifests and Cargo.lock package names to base `b5e7330`. Existing dependency tables
are intentional. Any changed entry or added lock package requires an explicit review
and guard update; the evidence report adds no dependencies. Fixture tests check that
an added dependency or [[package]] is rejected.
