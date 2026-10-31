# WAS-01 restricted cluster qualification

This fixture qualifies WAS-01 ac1/ac2 on the existing shared k3s cluster. It
approximates restricted-v2; it does not qualify OpenShift or record WAS-01 ac3.
`was-01/versions.toml` remains unverified. There is no weaker manifest, engine
switch or native fallback after failure.

Offline checks run with `cargo test --workspace --locked`. Without
`FS_TEST_CLUSTER=1`, the cluster test prints `UNVERIFIED` and contacts no cluster.
Never enable it in an unattended source implementation job.

An operator builds the project image from the repository root:

```sh
podman build -f worker/qualification/Containerfile -t <approved-registry>/was01:<build-id> .
```

Publish through the existing trusted image process and use its immutable digest.
No tenant binary or precompiled Wasmtime artifact is accepted. The runner embeds
both reviewed guest fixtures. Its image files are executable/readable by arbitrary
UIDs and are not writable. Supply KUBECONFIG through the existing OpenBao External
Secrets setup; never paste, print or commit its contents or installer tokens.

## Namespace modes

`FS_TEST_NAMESPACE_MODE=create` is the default, including when unset. Before any
mutation the admin must pass a namespace-create SelfSubjectAccessReview (SSAR).
Denial is UNVERIFIED and names `FS_TEST_NAMESPACE_MODE=shared`; the harness never
falls back automatically. The harness creates a unique `fs-test-was01-*` namespace
with `pod-security.kubernetes.io/enforce=restricted` and deletes it on completion,
error or unwinding panic. Cleanup verifies NotFound or Terminating.

`FS_TEST_NAMESPACE_MODE=shared` requires an operator-authorized existing
`FS_TEST_NAMESPACE=fs-test-<name>`. Shared cluster use requires accepted ADR 0011
or explicit operator authorization for that namespace; this source change grants
neither. The namespace must exist and enforce restricted admission. Only
`fs-test-*` DNS labels are accepted; `default`, `kube-system`,
`in-falcone-staging`, `family-rentals-prod`, `musematic*` and `llmwiki*` are refused.
No namespace is created, patched, labelled or deleted. Each created object has a
random run suffix and `falcone.test/run=<run-id>`. Creation is POST-only, so name
collisions are refused rather than adopted. Cleanup selects only this run's
labelled objects and verifies that none remain. Never remove foreign objects to
make the test pass.

An authorized operator runs one test (choose the mode explicitly when shared):

```sh
FS_TEST_CLUSTER=1 FS_TEST_NAMESPACE_MODE=create \
  FS_TEST_WAS01_IMAGE=<approved-registry>/was01@sha256:<digest> \
  cargo test -p falcone-serverless-worker --test was01_cluster --locked -- --nocapture
```

For shared mode also supply `FS_TEST_NAMESPACE_MODE=shared` and
`FS_TEST_NAMESPACE=fs-test-<authorized-name>`. KUBECONFIG must already be set; its
value must not appear in shell history or evidence.

## Evidence and failure handling

The five phases are namespace preflight/setup, admin bootstrap, admin TokenRequest,
installer-only fixture/SSARs/runtime evidence, and admin cleanup. Bootstrap creates
only an installer ServiceAccount, Role and RoleBinding. The installer Role grants
create/get/delete on serviceaccounts and deployments, get/list/watch on pods and
get on pods/log. It grants no RBAC writes, token requests or cluster-scoped writes.
All manifests pass local namespace, ownership and restricted-posture checks before
creation. Admission evidence contains only kind/name/UID/resourceVersion, never
raw API responses or credentials. Forbidden bootstrap or TokenRequest fails as
UNVERIFIED and still cleans up.

The Deployment is one-shot. Exit code 0 with all nine complete runner JSON records
from the **first** container run is accepted as startup evidence, even when a poll
misses its transient Ready condition. Any restart, nonzero exit, missing/failed/
unverified check, wrong effective UID, timeout or admission error fails the test.
The test records compile/load/executable-memory, result 42, epoch/fuel interruption,
root filesystem read-only and writable `/tmp` evidence. The chosen nonzero UID
differs from image USER 65532. Installer SSARs must deny create on clusterroles,
clusterrolebindings, runtimeclasses and namespaces.

Unset `FS_TEST_CLUSTER` as the kill switch. Revert the source change through the
normal review process to remove the fixture. SIGKILL can bypass Drop cleanup:
after a create run confirm its namespace is absent or Terminating; after a shared
run confirm no objects with the printed run label remain. An operator can remove
leftovers by the exact namespace or run label, after checking ownership. Cleanup
failures are reported, never treated as qualification success. Images remain in
the approved registry. R3 requires independent second review and operator approval
before merge. This runbook neither merges nor deploys anything.
