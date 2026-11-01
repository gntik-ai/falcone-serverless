# falcone-serverless

Falcone-oriented serverless execution for Kubernetes and OpenShift.

## Status

Design and implementation backlog. Option A selects Wasmtime/WASI execution inside restricted pods. Runtime implementation, cluster qualification and performance validation are pending.

## Layout

- `worker/`: Rust execution worker (Wasmtime/WASI host), a placeholder until RUN-01.
- `controller/`: Go namespaced pool controller, a placeholder until DEP-03.
- `docs/`: design, requirements and backlog.
- [AGENTS.md](AGENTS.md): code ownership across repositories, hard rules, build/test and cluster-testing rules.

## Documentation

- [Complete design and requirements](docs/falcone-serverless-design.md)
- [Backlog: 16 epics and 58 stories](docs/backlog.md)
- [Machine-readable requirements and issue payloads](docs/backlog.json)
- [Authoring sources and import instructions](docs/README.md)
- [GitHub issues](https://github.com/gntik-ai/falcone-serverless/issues)

The design preserves Falcone-specific contracts and integrates with Keycloak, SeaweedFS and Temporal. Untrusted code runs through qualified Wasm profiles; long-running work uses checkpoint/resume orchestration.

## License

MIT, the same as Falcone. See [LICENSE](LICENSE).

## WAS-01 restricted cluster qualification

The WAS-01 ac1/ac2 fixture is opt-in. Offline `cargo test --workspace --locked`
prints `UNVERIFIED` for its cluster test; this does not qualify a cluster or an
OpenShift SCC. The fixture uses the existing engine and import-free guests, with
no backend switch or native fallback. R3 requires independent second review and
explicit operator approval before merge.

The operator builds `worker/qualification/Containerfile` from the repository root
and supplies its pushed digest as `FS_TEST_WAS01_IMAGE=<mirror/repo>@sha256:<digest>`.
The image uses the repository's pinned bases, builds with `--locked`, embeds the
guests and runs as UID 65532. The harness chooses a different arbitrary non-root
UID. Image builds/pushes and runtime qualification are operator-run, not CI steps.

Supply `KUBECONFIG` through the existing OpenBao/External Secrets process; never
print or commit it. Set `FS_TEST_CLUSTER=1` and run just this fixture:

```sh
cargo test --locked -p falcone-serverless-worker --test was01_cluster was01_cluster -- --exact --nocapture
```

`FS_TEST_NAMESPACE_MODE=create` is the default, including when unset. Before any
mutation an admin SelfSubjectAccessReview must allow namespace creation. The test
then creates an `fs-test-was01-<random>` namespace with
`pod-security.kubernetes.io/enforce=restricted`, and deletes it on success,
failure or panic. A denied or unavailable review reports `UNVERIFIED` and names
the alternative switch before any mutation.

With `FS_TEST_NAMESPACE_MODE=shared`, set `FS_TEST_NAMESPACE` to an existing,
active `fs-test-*` namespace labelled `pod-security.kubernetes.io/enforce=restricted`.
The test refuses missing/unlabelled namespaces and all non-test namespaces,
including `in-falcone-staging`, `kube-system`, `default`, `family-rentals-prod`,
`musematic*` and `llmwiki*`. It never creates, modifies or deletes the shared
namespace. Every bootstrap/fixture object has a random run suffix and
`falcone.test/run=<run>` label. Cleanup re-reads each object's label and deletes
only this run's objects, with UID/resourceVersion preconditions to prevent races.
It never adopts or patches existing objects. New cluster suites can reuse
`worker/tests/support/test_namespace.rs` with the existing `NamespaceMode` and
`Namespace` types. The earlier worker-image `Kube`/`Session` interface is preserved.

In both modes the admin creates only the installer ServiceAccount, Role and
RoleBinding, then issues a short-lived TokenRequest. Only the installer creates
the workload ServiceAccount and Deployment. Its SelfSubjectAccessReviews must
deny cluster-scoped writes. The harness records admission responses, watches
from a resourceVersion captured before fixture creation, and requires observed
`Ready=True` with `lastTransitionTime`, no restarts, a successful runner exit,
compile/load, guest output 42, epoch/fuel interruption, the chosen effective UID,
a denied root-filesystem write, and verified `/tmp` payload readback. Failures
never relax posture or retry with admin fixture credentials.

Unset `FS_TEST_CLUSTER` as the kill switch. Cleanup failures are reported; after
a killed process the operator removes only the leftover run's namespace in
create mode, or its labelled objects in shared mode. Never delete the shared
namespace. Runtime results remain unverified until the operator runs both modes
on the shared cluster with a project-built digest-pinned image.
