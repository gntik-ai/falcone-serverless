# falcone-serverless: Option A implementation specification

Updated 27 October 2026. **Architecture direction accepted by the user; implementation, corporate-cluster feasibility and security qualification remain unverified.** This revision supersedes the initial native-container execution design. [ADR 0010](adr/0010-oci-execution-mode-and-base-catalog.md), accepted by the operator on 27 October 2026, adds OCI as an explicit execution mode that the platform superadmin authorizes per tenant and project, and a global superadmin base catalog. OCI mode is chosen per function and is never a silent fallback. The original survey is retained as comparative evidence.

## Accepted direction and scope

Build a Falcone-owned **WebAssembly/WASI execution layer inside ordinary restricted Kubernetes/OpenShift pods**, using Wasmtime as the sandbox engine. Keep Keycloak, SeaweedFS, Temporal, APISIX, Kafka, PostgreSQL and OpenBao. Falcone owns APIs, quotas, execution policy, integrations, routing and lifecycle.

The product supports untrusted code through explicitly qualified language and package profiles. In Wasm mode it does **not** accept arbitrary tenant Linux containers, native executables, native install scripts or unrestricted operating-system access. Existing Functions APIs, URLs and data contracts remain the compatibility target; raw Knative APIs and URLs are excluded.

Functions run in Wasm by default. A superadmin may authorize OCI mode per tenant (none, all or selected projects) under ADR 0010, as an explicit per-function choice with accepted same-kernel risk. There is still no silent fallback. OCI function images are built FROM a catalog base; a project may bring its own image only when the superadmin flags it. A project is a Falcone workspace.

**Compatibility is a measured gate.** Unsupported legacy dependencies block those workloads from migration. Selecting Option A is not proof that every existing function can run under Wasm. Neither silent native fallback nor weakening sandbox boundaries is an acceptable compatibility mechanism.

**Namespace placement proposal, not an active hard-rule change.** [ADR 0011](adr/0011-namespace-placement-mode.md) remains Proposed (R3); create/shared placement requires acceptance, a second reviewer and operator approval, plus the operator-applied AGENTS.md amendment. The documented defaults remain `namespaceMode=preprovisioned` for runtime workloads and `testNamespaceMode=create` for test fixtures. Test placement depends only on `testNamespaceMode`, independently of runtime placement; proposed shared tests use `FS_TEST_NAMESPACE` with owned per-run resources (#121). Code and chart support follow separately.

### Confirmed requirements

| Topic | Accepted requirement |
| --- | --- |
| Motivation | Corporate OpenShift restrictions make operators and special permissions difficult; prioritize namespace-only deployment, performance, Falcone integration and extensibility |
| Execution location | Option A inside restricted pods; no administrator-provided sandbox runtime. Authorized OCI functions use a tenant-bound OCI namespace, administrator-provided under default namespaceMode=preprovisioned (ADR 0010); Proposed ADR 0011 describes dedicated namespaces in create after permission preflight, or the release namespace in shared with an additional per-tenant superadmin gate and verified NetworkPolicy enforcement |
| Deployment permissions | No required operator, CRD, ClusterRole, custom RuntimeClass, elevated SCC or host access; default namespaceMode=preprovisioned uses approved existing namespaces and pregranted namespaced RBAC. Proposed ADR 0011 adds shared in the release namespace and create only after preflight proves namespace/RBAC creation authority |
| Tenant trust | Arbitrary untrusted code within qualified Wasm profiles; no arbitrary native execution. OCI mode: untrusted code in catalog-derived images, only in superadmin-authorized projects, with accepted same-kernel residual risk (ADR 0010) |
| Base catalog | Global, superadmin-managed catalog of Wasm runtime profiles and OCI base images, with tenant/project visibility; bases are selected by registry digest or created from a recipe |
| OCI authorization | Superadmin-only, per tenant (none, all or selected projects), audited; tenant admins cannot change it; BYO images only in projects the superadmin flags |
| Scale | Approximately 20 tenants and 10,000 deployed function definitions |
| Messaging | Approximately 100,000 aggregate WebSocket/event messages per second; mostly messaging, not 100,000 function invocations/second |
| Execution quota | Initial configurable estimate of 20 simultaneous executions per tenant; roughly 400 total if all 20 tenants use their quota |
| Analysis | Extended asynchronous jobs, configurable duration limits, progress, cancellation and application checkpoint/resume |
| Kubernetes | Latest two qualified stable minor lines, currently 1.37 and 1.36 in the research snapshot; patch-pinned releases |
| OpenShift | 4.20 / Kubernetes 1.33 family; exact OpenShift patch bundling the user's Kubernetes 1.33.12 must be confirmed from cluster metadata |
| SeaweedFS | Latest two qualified stable releases, currently 4.48 and 4.47 in the research snapshot; preserve the existing notification configuration when supplied |
| Transport | Existing WebSockets are mandatory; actual deployed Model Context Protocol behavior must be inventoried, not inferred from the ambiguous term “MCP” |
| Quotas | Definitions, accounting and charging policies are owned directly by Falcone |

20 execution slots per tenant are a quota assumption, not enough information to size compute. Required concurrency is approximately invocation rate times mean running duration, plus headroom. Function-trigger fraction, fan-out and duration distributions are still needed. WebSocket sessions and message forwarding do not inherently occupy guest execution slots.

### Evidence and boundaries

The prior targeted repository review used commit `bde62dd1e2e91f957b1dc6b8212dfbbaee977a7f`; no new live deployment inspection is claimed.

- Falcone's existing Node.js services, identity/plan policies and integration adapters remain the product facade. [S1]
- The source-based `main(params)` runtime and response envelope need explicit Wasm compatibility tests. [S2]
- The existing Temporal `functions.invoke` Activity's workspace binding and failure mapping remain mandatory. [S3]
- Existing managed-Knative documentation distinguishes source implementation from deployed acceptance. This replacement retains that evidence discipline. [S4–S5, S9]

Original work can follow Falcone's existing license policy, while exact pinned engine/toolchain/package licenses remain part of release review.


## 1. Survey and comparative analysis

### Knative components and capabilities

| Component | Responsibility |
| --- | --- |
| Serving resources | Service coordinates Configuration, immutable Revision and Route resources; supports revision rollouts and weighted traffic. |
| Serving controller and webhooks | Reconcile resources and validate/default configuration. |
| Autoscaler | Adjusts replicas from demand; Knative Pod Autoscaler supports request/concurrency-driven scaling and scale-to-zero. HPA support is a different scaling mode. |
| Activator | Buffers requests during activation and some burst conditions, requests scale-up, then forwards to ready capacity. |
| Queue-proxy | Per-pod request accounting and concurrency enforcement. |
| Networking adapter | Kourier, Contour or Istio implements Knative's ingress abstraction. Kourier is a lightweight Envoy-based adapter, not a required component of every Knative installation. |
| Eventing | Sources, Brokers, Triggers, Channels and Subscriptions connect CloudEvents producers/consumers; broker/channel implementations determine persistence and delivery properties. |
| Functions tooling | Source templates, local development and build/deploy tooling; distinct from the core Serving execution machinery. |

Serving architecture and networking are documented upstream. [S6–S7] Event retry and dead-letter behavior are separately configured; Eventing does not imply exactly-once function side effects. [S8]

### Falcone-specific pain points

**Verified integration concerns:** installation ownership spans cluster-scoped resources; coordinated chart/application acceptance is unfinished in the reviewed proposal; OpenShift admission requires explicit compatibility work; runtime outages must preserve tenant-safe errors and durable cleanup. These are recorded in Falcone's design/status documents. [S4–S5, S9]

**Architectural costs to measure:** overlapping control planes, additional proxies, cold-start delays, and the effort to map Knative resources to Falcone roles/quotas. These are plausible costs, not measured defects in the current deployment. A custom implementation will inherit activation, routing, fairness, draining and lifecycle responsibilities.

Do not assume Eventing is installed: the reviewed managed-Serving design explicitly excludes it. Retain the existing Kafka/scheduling integrations unless deployment discovery proves an additional Eventing dependency. [S5]

### Alternatives

Four established open-source alternatives are **Fission, Apache OpenWhisk, Nuclio and Fn Project**. They are mature architectural reference points, but not equally suitable or equally validated for Falcone's cluster matrix. OpenFaaS is included separately because its current distribution/license boundaries matter. Kubeless is a historical reference, not an actively maintained shortlist candidate.

| Platform | Features and scalability | Extension mechanisms | Fit for Keycloak / SeaweedFS / Temporal / tenancy | Strength and weakness versus Knative |
| --- | --- | --- | --- | --- |
| **Knative — baseline** | Container services; revisions; weighted routes; request/concurrency scaling; activation from zero; separate eventing ecosystem | Networking adapters, event sources/brokers, container contract | Falcone must supply business authorization, S3 credentials/events and Temporal Activities; Kubernetes boundaries need additional tenant controls | Broad Serving behavior and ecosystem; substantial platform machinery |
| **Fission — Apache-2.0** | Source functions, containers, HTTP/timer/message triggers; warm environment pools or per-function Deployments/HPA | Environment images, builders, CRDs and trigger adapters | Keycloak through Falcone ingress; S3 SDK/bindings and storage-event bridge; Temporal via invocation adapter; multi-namespace facilities still need Falcone authorization | Attractive source workflow and warm-pool design; pool cost/isolation and executor-specific behavior require validation |
| **Apache OpenWhisk — Apache-2.0** | Actions, activations, packages, feeds, triggers/rules and sequences; controllers/invokers schedule work and reuse containers; explicit limits | Language runtimes, packages/feeds, implementation SPIs | Auth/namespace translation at Falcone facade; S3 through actions/feed adapter; Temporal invokes actions and remains workflow owner; namespace quotas are useful but not Falcone roles | Rich FaaS accounting/events; heavier services and a different activation/runtime model |
| **Nuclio — Apache-2.0 core** | HTTP and streaming/broker triggers, language runtimes, function processors, concurrent workers and Kubernetes replica scaling | Runtime and trigger implementations, function config and custom images | Keycloak at Falcone boundary; S3 SDK/custom adapter; Temporal invocation bridge; projects/namespaces need security hardening | Useful event/data-processing architecture; edition/deployment-specific auth and scale-to-zero must be verified |
| **Fn Project — Apache-2.0 core** | Container functions, applications/triggers, CLI and language FDKs; execution agents and load-balancing agent support distributed capacity | FDKs, containers and Go extension hooks | External IAM facade; S3 client/config integration; Temporal adapter; application grouping alone is not tenant isolation | Simple container-oriented model; more Kubernetes lifecycle/routing work and release/support qualification than Knative |
| **OpenFaaS — current edition restrictions** | Templates, image functions, sync/async invocation; scaling, tenancy and production features depend on edition | Templates, watchdog, provider and connector interfaces | Falcone adapters still required; multi-namespace and advanced scaling availability must be checked by edition | Strong developer ergonomics; current CE is restricted, with production/commercial and scale-to-zero boundaries |
| **Kubeless — archived** | Historically Kubernetes-native functions, runtimes and triggers | CRDs/runtimes | Would require maintaining the platform and all Falcone adapters | Useful historical simplicity; reject as a new production foundation |

Fission's executor types and architecture are documented in [S10–S11]. OpenWhisk's action/namespace model and service architecture are documented in [S12–S14]. Nuclio's processor and trigger model is documented in [S15–S16]. Fn's runtime and agent mechanisms are documented in [S17–S18].

**Licensing correction:** current OpenFaaS documentation describes CE restrictions, including a commercial trial limit; advanced scaling/scale-to-zero is edition-dependent. Do not classify its complete current Kubernetes distribution as an unrestricted OSS dependency. Review individual pinned repositories, binaries and licenses before reuse. [S19–S20] Kubeless explicitly states that it is no longer actively maintained. [S21]

**Selection:** retain Knative as the migration reference; use Fission as the closest benchmark for source deployment and warm capacity, OpenWhisk for activation accounting, Nuclio for event adapters, and Fn for runtime portability. None is a drop-in combination of Falcone's IAM, object-store and Temporal contracts.

