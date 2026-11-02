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

## Releasing images

The official images are published to Docker Hub by `.github/workflows/release-images.yml`:

- `docker.io/gntik/falcone-serverless-worker`
- `docker.io/gntik/falcone-serverless-controller`

To release:

1. Push a strict `vX.Y.Z` tag (for example `git tag v0.1.0 && git push origin v0.1.0`), or publish a GitHub Release for such a tag.
2. Each image gets the tags `X.Y.Z` and `sha-<commit>`. A tag push also moves `:latest`.
3. A version or `sha-` tag that already exists is never overwritten. An existing pair that resolves to one digest is verified and kept; any other combination fails the run.
4. Read the digests from `release-images.json`, which lists each image's reference and `sha256` digest:
   - in the run summary;
   - as the run artifact `release-images`;
   - for a published Release, as a Release asset.

   The Falcone chart pins the images by digest from this file.

A manual run (Actions → release-images → Run workflow) takes a version and builds without pushing while `dry_run` is checked.

The run needs:

- the repository secrets `DOCKERHUB_USERNAME` and `DOCKERHUB_TOKEN`;
- both Docker Hub repositories created as public repositories beforehand. The workflow never creates them or changes their visibility.

## Cluster tests (opt-in)

Cluster tests never run by default: without `FS_TEST_CLUSTER=1` they print `UNVERIFIED` and make no client or `kubectl` call. They share the k3s cluster (Pod Security `restricted`, seccomp `RuntimeDefault`, read-only root filesystem, arbitrary non-root UID) and never touch a namespace other than their own `fs-test-*` one. See [AGENTS.md](AGENTS.md).

| Variable | Meaning |
|---|---|
| `FS_TEST_CLUSTER=1` | Opt in. Anything else disables every cluster test. |
| `KUBECONFIG` | Colon-separated list of kubeconfig files, from OpenBao through External Secrets. Every entry must be an existing, readable file. An empty entry (`:`, `::`, `/file:`), an empty value or an unset variable is reported as UNVERIFIED before any API or `kubectl` call. There is never a fallback to `~/.kube/config` or the in-cluster configuration, and every `kubectl` call carries `--kubeconfig` for each listed file (`kubectl` itself honours only the last `--kubeconfig` flag, so list a single merged file for `kubectl`-based tests). Contents are never printed. |
| `FS_TEST_IMAGE_REPO` | Untagged repository the worker-image harness (`cluster_harness`) builds and pushes to. |
| `FS_TEST_WAS01_IMAGE` | Digest-pinned runner image (`<repo>@sha256:<digest>`) built from `worker/qualification/Containerfile`, for `was01_cluster`. |
| `FS_TEST_NAMESPACE_MODE` | `create` (the default when unset) or `shared`. |
| `FS_TEST_NAMESPACE` | Required in `shared` mode: the existing `fs-test-*` namespace to use. |

Namespace modes (every cluster test gets its namespace from `test_namespace(suite)` in `worker/tests/support`):

- `create` (default): the test creates `fs-test-<suite>-<random>` labelled `pod-security.kubernetes.io/enforce=restricted` and deletes it afterwards, also on failure and panic. A SelfSubjectAccessReview first checks that the identity may create namespaces; if not, the run is UNVERIFIED before any mutation and the message names `FS_TEST_NAMESPACE_MODE=shared`.
- `shared`: no namespace is created or deleted. `FS_TEST_NAMESPACE` must be set, exist, carry `enforce=restricted` and be an `fs-test-*` name (the off-limits namespaces are refused), otherwise the run is UNVERIFIED before any mutation. Every object gets a per-run name suffix and the label `falcone.test/run=<random>`; cleanup deletes only objects with this run's label, and the harness never modifies or deletes an object without it.

Run, for the operator only: `FS_TEST_CLUSTER=1 KUBECONFIG=<files> FS_TEST_WAS01_IMAGE=<ref> cargo test --workspace --locked`. `SIGKILL` can bypass cleanup; afterwards look for leftover `fs-test-*` namespaces or objects labelled `falcone.test/run`. To stop all cluster activity, unset `FS_TEST_CLUSTER`.

WAS-01 (`worker/tests/was01_cluster.rs`) runs five phases: the test-admin acquires the namespace, applies the bootstrap RBAC in `worker/qualification/manifests/bootstrap` and requests a token for the installer ServiceAccount; only the installer applies `worker/qualification/manifests/fixture` and observes it; the namespace guard cleans up. The runner exits after its checks, so the pod's `Ready` condition is observed while the container runs and the pod restarts afterwards; this is expected.

## License

MIT, the same as Falcone. See [LICENSE](LICENSE).
