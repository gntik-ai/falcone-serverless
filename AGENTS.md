# falcone-serverless: agent and contributor guide

Falcone-owned WebAssembly/WASI execution layer ("Option A", [design](docs/falcone-serverless-design.md)):
- Wasmtime runs inside ordinary restricted Kubernetes/OpenShift pods.
- Falcone keeps the APIs, quotas, execution policy, integrations, routing and lifecycle.
- It reuses Keycloak, SeaweedFS, Temporal, APISIX, Kafka, PostgreSQL and OpenBao.

## Where code lives (operator decision, 2026-10-22)

| Repository | Owns |
|---|---|
| `gntik-ai/falcone-serverless` (this repo) | the Rust execution worker (`worker/`, embeds a pinned Wasmtime) and the Go namespaced pool controller (`controller/`) |
| `gntik-ai/falcone` | the public Functions API, quotas, admission/dispatch, console, SDK and the `functions.invoke` Temporal activity |
| `gntik-ai/falcone-charts` | the Helm chart that deploys the worker pools and the controller |

A story that spans repositories is split into one issue per repository, each with a single owner.

## Hard rules (design §6; do not relax without an accepted ADR)

- Restricted pods only: no operator, CRD, ClusterRole, custom RuntimeClass, elevated SCC or host access. The controller writes only standard Deployments, Services and Leases in preprovisioned namespaces.
- No tenant OCI or native binaries, no Dockerfiles or install hooks, and no native fallback. Unsupported workloads are blocked from migration.
- A tenant-supplied precompiled Wasmtime artifact is never deserialized. Native code caches come only from trusted build components.
- Guests reach the platform only through the versioned WIT/WASI capability host (storage, allow-listed HTTP, events, workflows, logging). Secrets stay in the host binding layer.
- Security-sensitive areas are the sandbox, host capabilities, egress, secrets and tenant isolation. A second reviewer and explicit operator approval are needed (Hermes R3).

## Build and test

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
(cd controller && test -z "$(gofmt -l .)" && go vet ./... && go test ./...)
```

The Rust toolchain is pinned in `rust-toolchain.toml` and Go in `controller/go.mod`. CI (`.github/workflows/ci.yml`) runs the same commands. Agents do not edit `.github/`; the operator owns it.

## Cluster testing (operator decision, 2026-10-22)

- Every runtime test runs on the shared cluster, in dedicated `fs-test-*` namespaces created and deleted by the test.
- Integration with Falcone reads `in-falcone-staging`. Other namespaces are off limits, including `family-rentals-prod`, `musematic*` and `llmwiki*`.
- The cluster is k3s, not OpenShift. Pod Security "restricted" plus seccomp `RuntimeDefault`, a read-only root filesystem and an arbitrary non-root UID stand in for OpenShift restricted-v2. Real OpenShift qualification is a later, separate step.
- Never print or commit secrets. Credentials come from OpenBao through the existing External Secrets setup.

## Falcone findings

A bug or improvement found in Falcone, its charts or its platform services becomes an issue in that repository (gntik-ai/falcone or gntik-ai/falcone-charts). It is not worked around silently here.
