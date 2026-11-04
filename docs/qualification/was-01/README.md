# WAS-01 cluster evidence: Kubernetes version and namespaced permissions

This records what a probing identity could observe on a cluster for WAS-01 acceptance criteria
ac3 (exact versions and namespaced permissions, absent access is unverified) and ac1 (no
cluster-scoped writes beyond this run's own namespace). It is evidence input, not a verdict:
nothing deployed reads it, the report is never committed, and the story summary is always
`not-go`.

Wasmtime 49.0.5 is an unqualified candidate recorded in `worker/qualification/was-01/versions.toml`,
not a selected pin (ADR 0003 is still Proposed). `versions.toml` keeps its `unverified` values.

## Running it

Offline is the default. `cargo test --workspace --locked` runs `was01_cluster_evidence` without
any cluster contact and writes an all-unverified report under the target directory
(`CARGO_TARGET_TMPDIR`, for example `target/tmp/was01-evidence/`).

The operator opts in on the shared cluster:

```sh
FS_WAS01_CLUSTER=1 KUBECONFIG=<files> cargo test -p falcone-serverless-worker --locked \
  --test was01_cluster -- --nocapture was01_cluster_evidence
```

| Variable | Meaning |
|---|---|
| `FS_WAS01_CLUSTER` | Gate. Only the exact value `1` contacts a cluster. |
| `KUBECONFIG` | The kubeconfig files to probe with (colon separated). Required with the gate on; there is no ambient or in-cluster fallback. An unusable value is an error before any kubectl call. |
| `FS_TEST_NAMESPACE_MODE` | `create` (the default) is the only mode this run supports. `shared` is an error with the gate on: the run creates and deletes its own namespace. |
| `FS_TEST_NAMESPACE` | Only read to validate `FS_TEST_NAMESPACE_MODE=shared`, which is then refused. |

With the gate on, the run creates `fs-test-was01-<random>` (an `fs-test-*` DNS label of at most
63 characters), reads `/version`, asks `kubectl auth can-i` for `get`, `create`, `update` and
`delete` on `deployments.apps`, `services` and `leases.coordination.k8s.io` in that namespace,
then deletes it with `--wait`. The report path is printed. The run owns the namespace only if
its create succeeded: after a failed create (including `AlreadyExists`) nothing is deleted.

## Items

Every item starts `unverified`. `passed` needs evidence; a `no`, an error or anything unexpected
stays unverified.

| Item | Source | Passed evidence | Unverified reason codes |
|---|---|---|---|
| `kubernetes-version` | `get --raw /version` `gitVersion` | the version, verbatim (`v1.37.2`, `v1.36.4+k3s1`) | `not-probed`, `missing-version`, `malformed-version`, `unsupported-version`, `forbidden`, `probe-error` |
| `can-i-<verb>-<resource>` (12 items) | `auth can-i <verb> <resource> -n <run>` | exact `yes` | `not-probed`, `denied`, `probe-error` |
| `plan-namespaced-only` (ac1) | the call plan | `calls=<n>` | `not-probed` |

The twelve permission item IDs are `can-i-get-deployments.apps`, `can-i-create-deployments.apps`,
`can-i-update-deployments.apps`, `can-i-delete-deployments.apps`, `can-i-get-services`,
`can-i-create-services`, `can-i-update-services`, `can-i-delete-services`,
`can-i-get-leases.coordination.k8s.io`, `can-i-create-leases.coordination.k8s.io`,
`can-i-update-leases.coordination.k8s.io` and `can-i-delete-leases.coordination.k8s.io`.

These items never leave their fixed state in this task:

| Item | State |
|---|---|
| `openshift-4.20` | unverified, `openshift-not-available` |
| `pin-engine`, `pin-architecture`, `pin-wasi`, `pin-compiler` | unverified, `no-pin-selected` |
| `guest-install`, `guest-execute`, `guest-interrupt` | unverified, `not-collected` |

Other reason codes: `check-failed` and `plan-violation` (a `failed` item), and the cell codes
`item-failed`, `item-unverified` and `no-matching-server`.

### Reason codes

`not-probed`, `not-collected`, `no-pin-selected`, `openshift-not-available`, `missing-version`,
`malformed-version`, `unsupported-version`, `forbidden`, `denied`, `probe-error`, `check-failed`,
`plan-violation`, `item-failed`, `item-unverified`, `no-matching-server`.

`forbidden` is recognised only for `/version`, when kubectl's stdout carries `Forbidden`; stderr
is discarded, so any other failed call is `probe-error`.

## Targets

The server `gitVersion` fills one cell: `k8s-1.37` for `v1.37.x` or `k8s-1.36` for `v1.36.x`
(a `+build` suffix such as `+k3s1` is allowed). `v1.35.0`, `v1.370.1`, `v1.37.0-rc.1` and other
majors fill no cell and the version item is `unverified` (`unsupported-version`). A cell is
`failed` if any ac3 item failed, `passed` only if all 13 ac3 items passed, otherwise
`unverified`. The cell without a matching server is `unverified` (`no-matching-server`).
`openshift-4.20` is never filled: no OpenShift cluster is available.

## Report format

One line per fact: the header `was01-cluster-evidence 1`; `item <id> <verdict> <reason|-> <evidence|->`
for every item in a fixed order; `cell <target> <verdict> <reason|->` for each target; and
`summary not-go non-passed=<ids>`. Reading a report back rejects malformed lines, unknown IDs
and cell or summary lines that do not match the items. Cluster output is never copied into a
report or an error; a `gitVersion` is recorded only after a charset check.

## Limits you must keep in mind

- **k3s stands in for OpenShift.** The shared cluster is k3s (AGENTS.md). A k3s result is not
  OpenShift evidence; `openshift-4.20` stays unverified until a real OpenShift qualification.
- **Permissions reflect the probing identity.** An admin kubeconfig answers `yes` everywhere, so
  `passed` proves only that identity, not the deployment identity. A least-privilege identity
  cannot create the namespace, so this run cannot probe as one; probing as the deployment
  identity needs a separate task.
- **The namespace create is a cluster-scoped write by the test identity.** Only this run's own
  `fs-test-was01-*` namespace is allowed, and the plan guard accepts exactly these calls:
  `get --raw /version`, `create namespace <run>`, `delete namespace <run> --wait` and
  `auth can-i <listed verb> <listed resource> -n <run>`. Anything else (another namespace,
  `-A`, a missing `-n`, cluster-scoped kinds, `runtimeClassName`, `apply -f`) fails
  `plan-namespaced-only` before any call.
- **Orphaned namespaces need manual cleanup.** A create that timed out after succeeding on the
  server, a failed delete, or a crashed run (SIGKILL) leaves `fs-test-was01-*` behind. The error
  names the namespace; delete it with `kubectl delete namespace <name> --wait`. A run never
  deletes namespaces left by earlier runs.

## Dependency guard

`was01_cluster.rs` fails if `worker/Cargo.toml` gains a dependency entry or `Cargo.lock` gains,
loses or changes a package compared with `worker/tests/support/was01_dependency_baseline.txt`.
The baseline is a snapshot: after a reviewed dependency change it fails until refreshed with
`cargo test -p falcone-serverless-worker --locked --test was01_cluster -- --ignored --nocapture print_dependency_baseline`.
