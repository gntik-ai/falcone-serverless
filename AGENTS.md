# falcone-serverless: agent and contributor guide

Falcone-owned WebAssembly/WASI execution layer ("Option A", [design](docs/falcone-serverless-design.md)):
- Wasmtime runs inside ordinary restricted Kubernetes/OpenShift pods.
- Falcone keeps the APIs, quotas, execution policy, integrations, routing and lifecycle.
- It reuses Keycloak, SeaweedFS, Temporal, APISIX, Kafka, PostgreSQL and OpenBao.
- Wasm is the default runtime mode. OCI image mode is an explicit, superadmin-authorized mode ([ADR 0010](docs/adr/0010-oci-execution-mode-and-base-catalog.md)), never a fallback.

## Where code lives (operator decision, 2026-10-22)

| Repository | Owns |
|---|---|
| `gntik-ai/falcone-serverless` (this repo) | the Rust execution worker (`worker/`, embeds a pinned Wasmtime) and the Go namespaced pool controller (`controller/`) |
| `gntik-ai/falcone` | the public Functions API, quotas, admission/dispatch, console, SDK and the `functions.invoke` Temporal activity |
| `gntik-ai/falcone-charts` | the Helm chart that deploys the worker pools and the controller |

A story that spans repositories is split into one issue per repository, each with a single owner.

ADR 0010 adds the OCI shim, egress gateway, image intake and image assembler to this repository; the OCI authorization and base catalog to Falcone; and the OCI values and OCI-namespace NetworkPolicy projections to falcone-charts.

## Hard rules (design §6; do not relax without an accepted ADR)

- Restricted pods only, in both runtime modes: no operator, CRD, ClusterRole, custom RuntimeClass, elevated SCC or host access. The controller writes only standard Deployments, Services and Leases in preprovisioned namespaces.
- Wasm mode (the default): tenants supply no OCI or native binaries, Dockerfiles or install hooks, and there is no native fallback. Unsupported workloads are blocked from migration. Trusted platform images (worker, compiler, and the OCI shim, egress gateway, intake and assembler) are built by this project.
- A tenant-supplied precompiled Wasmtime artifact is never deserialized. Native code caches come only from trusted build components.
- Wasm mode: guests reach the platform only through the versioned WIT/WASI capability host (storage, allow-listed HTTP, events, workflows, logging). Secrets stay in the host binding layer, unless a reviewed, narrow grant exposes one to a guest (design §6).
- OCI mode ([ADR 0010](docs/adr/0010-oci-execution-mode-and-base-catalog.md)):
  - A function runs in OCI mode only when the platform superadmin has authorized its project (a Falcone workspace) for the tenant. Only the superadmin changes that setting. Falcone enforces it at deploy, promotion and every admission, and audits it. Nothing switches a function between modes automatically, and OCI is never a fallback.
  - Function images are built FROM an enabled catalog base that is visible to the project. Falcone never runs tenant Dockerfiles or build hooks. A project may bring its own image (BYO) only when the superadmin flags it, and a BYO image must be signed.
  - Every image is pinned by digest and copied into the platform intake mirror. Kubelets pull only from the mirror, never from tenant registries.
  - OCI pods run single-revision, under the restricted posture, in the tenant's preprovisioned OCI namespace. The controller's write set is unchanged.
  - Tenant containers reach Falcone capabilities only through the trusted shim's localhost capability API and the egress gateway. NetworkPolicy enforcement is mandatory; without it, OCI mode is blocked.
  - The execution credential never enters the tenant container. The container holds only an opaque per-attempt handle, and secrets arrive only as reviewed narrow grants.
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
- Integration tests may also read `in-falcone-staging` (Falcone's staging release, which the operator explicitly approved for this). Every other namespace is off limits, including `family-rentals-prod`, `musematic*` and `llmwiki*`.
- The cluster is k3s, not OpenShift. Pod Security "restricted" plus seccomp `RuntimeDefault`, a read-only root filesystem and an arbitrary non-root UID stand in for OpenShift restricted-v2. Real OpenShift qualification is a later, separate step.
- Never print or commit secrets. Credentials come from OpenBao through the existing External Secrets setup.

## Falcone findings

A bug or improvement found in Falcone, its charts or its platform services becomes an issue in that repository (gntik-ai/falcone or gntik-ai/falcone-charts). It is not worked around silently here.
