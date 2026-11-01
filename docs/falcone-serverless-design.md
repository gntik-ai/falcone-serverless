# falcone-serverless: Option A implementation specification

Updated 27 October 2026. **Architecture direction accepted by the user; implementation, corporate-cluster feasibility and security qualification remain unverified.** This revision supersedes the initial native-container execution design. [ADR 0010](adr/0010-oci-execution-mode-and-base-catalog.md), accepted by the operator on 27 October 2026, adds OCI as an explicit execution mode that the platform superadmin authorizes per tenant and project, and a global superadmin base catalog. OCI mode is chosen per function and is never a silent fallback. The original survey is retained as comparative evidence.

## Accepted direction and scope

Build a Falcone-owned **WebAssembly/WASI execution layer inside ordinary restricted Kubernetes/OpenShift pods**, using Wasmtime as the sandbox engine. Keep Keycloak, SeaweedFS, Temporal, APISIX, Kafka, PostgreSQL and OpenBao. Falcone owns APIs, quotas, execution policy, integrations, routing and lifecycle.

The product supports untrusted code through explicitly qualified language and package profiles. In Wasm mode it does **not** accept arbitrary tenant Linux containers, native executables, native install scripts or unrestricted operating-system access. Existing Functions APIs, URLs and data contracts remain the compatibility target; raw Knative APIs and URLs are excluded.

Functions run in Wasm by default. A superadmin may authorize OCI mode per tenant (none, all or selected projects) under ADR 0010, as an explicit per-function choice with accepted same-kernel risk. There is still no silent fallback. OCI function images are built FROM a catalog base; a project may bring its own image only when the superadmin flags it. A project is a Falcone workspace.

**Compatibility is a measured gate.** Unsupported legacy dependencies block those workloads from migration. Selecting Option A is not proof that every existing function can run under Wasm. Neither silent native fallback nor weakening sandbox boundaries is an acceptable compatibility mechanism.

**Namespace placement decision; rollout gates remain.** [ADR 0011](adr/0011-namespace-placement-mode.md) is Accepted (2026-11-01, [#237](https://github.com/gntik-ai/falcone-serverless/issues/237)): namespace creation stays available and is switchable to a shared, preprovisioned namespace. Create/shared rollout still requires a second reviewer and operator approval, plus the operator-applied AGENTS.md amendment. The documented defaults remain `namespaceMode=preprovisioned` for runtime workloads and `testNamespaceMode=create` for test fixtures. Test placement depends only on `testNamespaceMode`, independently of runtime placement; shared tests use `FS_TEST_NAMESPACE` with owned per-run resources (#121). Code and chart support follow separately.

### Confirmed requirements

| Topic | Accepted requirement |
| --- | --- |
| Motivation | Corporate OpenShift restrictions make operators and special permissions difficult; prioritize namespace-only deployment, performance, Falcone integration and extensibility |
| Execution location | Option A inside restricted pods; no administrator-provided sandbox runtime. Authorized OCI functions use a tenant-bound OCI namespace, administrator-provided under default namespaceMode=preprovisioned (ADR 0010); Accepted ADR 0011 describes dedicated namespaces in create after permission preflight, or the release namespace in shared with an additional per-tenant superadmin gate and verified NetworkPolicy enforcement |
| Deployment permissions | No required operator, CRD, ClusterRole, custom RuntimeClass, elevated SCC or host access; default namespaceMode=preprovisioned uses approved existing namespaces and pregranted namespaced RBAC. Accepted ADR 0011 adds shared in the release namespace and create only after preflight proves namespace/RBAC creation authority |
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



## 2. Functional requirements checklist
All 100 statements apply to the accepted Option A design as amended by ADR 0010: Wasm is the default runtime mode, and OCI mode runs only where the platform superadmin authorizes it. Milestones do not establish production qualification. Unsupported existing workloads block their migration; API compatibility does not imply arbitrary Linux-runtime compatibility.

### API. API and developer experience

- [ ] **FR-API-01:** The system must preserve inventoried Falcone Functions routes, API versions, request/response schemas, error codes, pagination and idempotency behavior through a compatibility adapter.
- [ ] **FR-API-02:** The system must provide authenticated REST and CLI operations to create, inspect, update, invoke, roll back and delete functions and inspect activations.
- [ ] **FR-API-03:** The system must provide JavaScript, Python and Go starters only for qualified Wasm/WASI profiles, plus an OCI starter built FROM a catalog base for OCI-authorized projects, with pinned supported dependencies and a local invocation command; show unsupported capabilities before deployment.
- [ ] **FR-API-04:** The system must accept a versioned declarative function manifest, including runtime mode, catalog base and OCI image fields; validate it, including OCI authorization and base visibility and state, before deployment and return field-specific errors without partial mutations.
- [ ] **FR-API-05:** The system must expose runtime readiness and deployment progress in the existing console with role-appropriate detail and accessible failure states.
- [ ] **FR-API-06:** The system must publish machine-readable OpenAPI schemas and run generated-client contract tests against every supported API version.

### RUN. Runtime and execution

- [ ] **FR-RUN-01:** The system must execute validated WebAssembly modules/components using a pinned Wasmtime worker and versioned Falcone host interfaces; in mode=wasm, reject arbitrary native tenant binaries and OCI container execution, which run only through the superadmin-authorized OCI mode of ADR 0010.
- [ ] **FR-RUN-02:** The system must preserve main(params), result/status/log envelopes and scoped invocation verification for qualified legacy functions; detect unsupported libraries or OS behavior and block migration rather than silently altering semantics.
- [ ] **FR-RUN-03:** The system must enforce guest memory, host-buffer, compiler, CPU, payload, execution-time and concurrency budgets; report the specific exceeded limit.
- [ ] **FR-RUN-04:** The system must provide worker readiness/startup probes, bounded draining, guest interruption and host-call cancellation; reject new executions on draining workers.
- [ ] **FR-RUN-05:** The system must in mode=wasm, create fresh guest state for each invocation; reuse only validated compiled code, never another invocation’s memory, file handles, credentials or temporary files. OCI mode follows the documented warm-reuse semantics of ADR 0010.
- [ ] **FR-RUN-06:** The system must publish a pinned engine/WASI/language/package support matrix and EOL policy; block unsupported combinations before deployment without silently changing revisions.

### DEP. Build, revisions and deployment

- [ ] **FR-DEP-01:** The system must in mode=wasm, build supported source into Wasm with approved toolchains and dependency catalogues, or accept validated raw Wasm artifacts; never execute tenant-provided native install hooks, Dockerfiles or build scripts in an unsandboxed host. OCI function images are assembled or verified FROM catalog bases without tenant build hooks.
- [ ] **FR-DEP-02:** The system must generate SBOM/provenance for function artifacts and trusted worker images, verify signatures, validate Wasm imports/features and pin every runtime/artifact dependency by digest.
- [ ] **FR-DEP-03:** The system must create an immutable revision for code, engine/profile, host-interface or effective configuration changes and retain all artifact references.
- [ ] **FR-DEP-04:** The system must deploy a revision only after readiness succeeds; support weighted canary traffic and atomic rollback to a retained revision.
- [ ] **FR-DEP-05:** The system must reconcile tenant worker pools and routing generations after restarts; represent deployed functions as metadata/artifacts without allocating one pod or Service per function, and materialize single-revision OCI pools only on demand within a per-tenant bound on materialized OCI revisions.
- [ ] **FR-DEP-06:** The system must garbage-collect obsolete images, revisions and build artifacts only after checking active routes, in-flight invocations, rollback retention and workflow pins.

### INV. Invocation, routing and autoscaling

- [ ] **FR-INV-01:** The system must route synchronous invocations by the pinned revision's runtime mode to authorized tenant-scoped Wasm workers or OCI revision pools, with a pinned immutable function revision and a configured end-to-end deadline.
- [ ] **FR-INV-02:** The system must accept asynchronous invocations only after durable persistence, return an activation ID, and expose terminal status and result retrieval.
- [ ] **FR-INV-03:** The system must enforce per-pod hard concurrency and tenant-wide admission limits across multiple router replicas; bound queue length and waiting time.
- [ ] **FR-INV-04:** The system must scale tenant/profile worker pools and single-revision OCI pools between configured minimum and maximum replicas from execution demand and admitted backlog; support idle scale-to-zero without tying replica count to deployed function count or raw message rate.
- [ ] **FR-INV-05:** The system must prevent scale-down from discarding admitted work, account for long-running jobs, use stabilization and metric freshness, and maintain exactly one scaling owner per worker pool.
- [ ] **FR-INV-06:** The system must preserve existing WebSocket URLs, authentication, ordering rules, backpressure, reconnect and draining semantics at a trusted gateway; separate connection/message capacity from guest execution slots.

### EVT. Events and triggers

- [ ] **FR-EVT-01:** The system must normalize Kafka, storage, webhook and scheduled events into versioned CloudEvents envelopes containing trusted tenant/workspace context.
- [ ] **FR-EVT-02:** The system must create, update, filter, pause and delete triggers with separate permissions for event sources and target functions.
- [ ] **FR-EVT-03:** The system must provide at-least-once durable delivery, configurable bounded retries/backoff, terminal dead-letter queues and audited manual replay.
- [ ] **FR-EVT-04:** The system must deduplicate delivery identifiers within a documented retention window and expose idempotency keys to function code without promising exactly-once external effects.
- [ ] **FR-EVT-05:** The system must support ordering per configured key/partition and bounded parallelism; document that delayed retries or replay can change order unless strict ordering is selected.
- [ ] **FR-EVT-06:** The system must integrate cron triggers with the existing Falcone scheduling service, retaining timezone, daylight-saving, overlap and missed-run policies.

### WF. Temporal integration

- [ ] **FR-WF-01:** The system must execute functions from Temporal Activities through the existing functions.invoke contract, preserving result and retryable/non-retryable error mappings.
- [ ] **FR-WF-02:** The system must assign a stable logical invocation key across retries of one Activity and record workflow, run, activity and attempt identifiers.
- [ ] **FR-WF-03:** The system must pin the function revision at the first durable admission of an Activity invocation and reuse that pin across retries.
- [ ] **FR-WF-04:** The system must choose one retry owner for each failure class; Temporal-owned retries must not multiply with hidden runtime delivery retries.
- [ ] **FR-WF-05:** The system must propagate heartbeats, cancellation and deadlines to guest and host operations; persist explicit progress/checkpoint references and report cancellation uncertainty rather than claiming external side effects were reversed.
- [ ] **FR-WF-06:** The system must allow authorized function code to start or signal allow-listed workflows via a scoped Falcone API, using object references for payloads above the agreed size budget.

### SEC. Identity and security

- [ ] **FR-SEC-01:** The system must validate Keycloak issuer, audience, signature, allowed algorithms and token lifetime using an explicit realm allow-list and bounded JWKS caching/rotation.
- [ ] **FR-SEC-02:** The system must authorize every management, invoke, log, result, secret and trigger operation using Falcone's verified tenant/workspace roles and capability policies; ignore spoofed identity headers.
- [ ] **FR-SEC-03:** The system must run trusted Falcone worker images under restricted non-root, arbitrary-UID-compatible policies without privilege escalation, host mounts/networking or automatic API tokens in guest workers.
- [ ] **FR-SEC-04:** The system must in mode=wasm, execute untrusted tenant code only in the qualified application-level Wasmtime sandbox; require no custom RuntimeClass, cluster CRDs, operator installation or elevated SCC, and never fall back to native or OCI execution. OCI mode runs only in superadmin-authorized projects, in restricted pods, under ADR 0010.
- [ ] **FR-SEC-05:** The system must authenticate service calls and issue short-lived, audience-bound execution credentials; keep signing keys and broad service credentials out of guests and scope each host operation to verified tenant/workspace context.
- [ ] **FR-SEC-06:** The system must enforce tenant network isolation and destination policies, block metadata/private-network SSRF except explicitly authorized bindings, and audit security-sensitive actions without recording secrets.

### TEN. Tenancy, plans and quotas

- [ ] **FR-TEN-01:** The system must persist tenant_id and workspace_id on every business resource, activation, artifact binding and audit record, with database enforcement and authorization-scoped lookups.
- [ ] **FR-TEN-02:** The system must use namespaceMode=preprovisioned by default with administrator-provided namespaces, explicit namespaced RBAC and verified tenant mappings; Accepted ADR 0011 describes create after namespace/RBAC permission preflight and shared in the release namespace, subject to the operator hard-rule amendment and R3 rollout approval. In shared mode, per-tenant names/labels and tenant-scoped NetworkPolicies become the tenant boundary (R3), with distinct service accounts and unchanged credential/RLS checks.
- [ ] **FR-TEN-03:** The system must obtain all quota and charging policies from Falcone, including failures, retries, startup, idle capacity, builds, storage, WebSocket connections/messages and interactive/batch execution.
- [ ] **FR-TEN-04:** The system must enforce aggregate reservations across replicas with an initial configurable allowance of 20 executions per tenant; allocate separate interactive/batch budgets and fair queued capacity.
- [ ] **FR-TEN-05:** The system must on tenant suspension or capability disable, deny new work immediately, pause triggers and apply an explicit policy to in-flight and queued work.
- [ ] **FR-TEN-06:** The system must on function/workspace/tenant deletion, persist cleanup obligations, verify ownership before deletion and report completion only after all scoped runtime resources are absent.

### STO. SeaweedFS, secrets and metadata

- [ ] **FR-STO-01:** The system must store source, Wasm artifacts, checkpoints and large payloads/results in SeaweedFS through existing scoped storage APIs with checksums, retention and authorized references.
- [ ] **FR-STO-02:** The system must test S3 operations against the latest two qualified stable SeaweedFS releases, initially 4.48 and 4.47 from the research snapshot, preserving current notification configuration after it is supplied and verified.
- [ ] **FR-STO-03:** The system must translate verified storage events into function triggers, including writes made through direct S3 clients, and document source-side retention and recovery guarantees.
- [ ] **FR-STO-04:** The system must resolve OpenBao secrets through host capabilities or explicit narrow guest bindings; keep values out of source-controlled manifests, artifacts and logs and support scoped rotation.
- [ ] **FR-STO-05:** The system must persist desired function state, activation state, deduplication records and cleanup obligations in PostgreSQL with migrations and unique scope-aware keys.
- [ ] **FR-STO-06:** The system must use transactional outbox/inbox patterns and fenced attempt ownership so database/broker failures do not lose accepted work or allow stale workers to overwrite terminal status.

### OBS. Observability, audit and metering

- [ ] **FR-OBS-01:** The system must emit structured logs and trace context carrying trusted tenant/workspace, function/revision, activation and workflow identifiers, with tenant-scoped retrieval.
- [ ] **FR-OBS-02:** The system must export OpenTelemetry traces across gateway, admission, event dispatch, runtime and workflow boundaries, preserving W3C context when permitted.
- [ ] **FR-OBS-03:** The system must measure deployed function count, connected WebSockets, ingress/egress messages, triggered invocations, execution/queue/activation latency, guest/host memory, pool capacity, retries and DLQ depth separately with bounded-cardinality metrics.
- [ ] **FR-OBS-04:** The system must provide dashboards and alerts for unavailable runtimes, saturation, scaling failure, stale queues, security denials and failed cleanup.
- [ ] **FR-OBS-05:** The system must record append-only auditable lifecycle/security events and invocation usage units with stable event IDs and reconciliation.
- [ ] **FR-OBS-06:** The system must enforce telemetry redaction, configurable retention and per-tenant access controls; never use unbounded user input as metric labels.

### OPS. Operations, CI/CD and resilience

- [ ] **FR-OPS-01:** The system must install with ordinary namespaced Helm resources and pregranted RBAC on the latest two qualified Kubernetes minor lines and OpenShift 4.20, with namespaceMode=preprovisioned by default; Accepted ADR 0011 describes shared without namespace creation and an explicit create exception for dedicated namespaces only after namespace/RBAC permission preflight, subject to the operator amendment and R3 rollout approval. Require no CRD, ClusterRole, operator, custom RuntimeClass or elevated SCC creation in any mode.
- [ ] **FR-OPS-02:** The system must support HA control/data-plane replicas, leader election or sharding for singleton duties, disruption budgets and documented degraded behavior.
- [ ] **FR-OPS-03:** The system must provide CI gates for API contracts, tenant isolation, runtime conformance, supply-chain policy and realistic integration tests with Keycloak, SeaweedFS, Kafka and Temporal.
- [ ] **FR-OPS-04:** The system must submit function manifests through the authoritative Falcone API; keep PostgreSQL as desired-state authority and let one namespaced reconciler own standard worker-pool resources without custom Kubernetes resources.
- [ ] **FR-OPS-05:** The system must provide backup/restore and disaster-recovery procedures covering PostgreSQL, SeaweedFS, broker state, secret references and runtime reconstruction, with measured RPO/RTO.
- [ ] **FR-OPS-06:** The system must support compatible upgrades, schema evolution, rollback or forward-repair boundaries, installation preflight and retain-by-default uninstall.

### MIG. Compatibility and Knative retirement

- [ ] **FR-MIG-01:** The system must freeze Falcone API/URL/data/authorization/execution contracts and classify existing functions by Wasm language/package compatibility; exclude raw Knative APIs and URLs from the replacement scope.
- [ ] **FR-MIG-02:** The system must record the accepted Option A decision as amended by ADR 0010 (superadmin-authorized OCI mode and base catalog); supersede silent native-fallback, CRD-controller and administrator-installed sandbox assumptions; distinguish design approval from implementation/security qualification.
- [ ] **FR-MIG-03:** The system must implement interchangeable Knative and native backend adapters with per-resource backend selection, an explicit target runtime mode and exactly one active traffic/trigger owner.
- [ ] **FR-MIG-04:** The system must preserve public identifiers, URLs, permissions, versions and activation history during migration; never double-execute side-effecting shadow traffic.
- [ ] **FR-MIG-05:** The system must validate qualified Function lifecycle, WebSocket behavior, Flows, failures, isolation and any actually deployed MCP contracts before cutover; list unsupported legacy workloads and block their migration, and move a workload to OCI mode only by explicit choice with an oci-candidate verdict in an authorized project.
- [ ] **FR-MIG-06:** The system must retire Knative/Kourier only after a dependency inventory is empty, in-flight and retained-workflow references are resolved, rollback policy is met and resource ownership is verified.

### WAS. Wasm qualification and capability contract

- [ ] **FR-WAS-01:** The system must demonstrate Wasmtime startup, guest execution, interruption and compilation under the existing OpenShift SCC and namespace identity without additional administrator privileges.
- [ ] **FR-WAS-02:** The system must pin engine, architecture, WASI interface and compiler versions; test qualified Kubernetes 1.37/1.36 lines and the exact OpenShift 4.20 patch reported by the operator, without assuming CRI-O patch numbers identify Kubernetes patches.
- [ ] **FR-WAS-03:** The system must define versioned, capability-controlled Falcone storage, HTTP, event and workflow host interfaces; deny undeclared imports and ambient filesystem/network authority.
- [ ] **FR-WAS-04:** The system must keep untrusted raw Wasm validation, bounded compilation and trusted native cache generation separate; never deserialize a tenant-supplied native compiled artifact.
- [ ] **FR-WAS-05:** The system must publish exact JavaScript, Python and Go Wasm-mode profile/package capabilities and compatibility diagnostics; reject unsupported native libraries, GPU access, process spawning or install hooks in mode=wasm unless a future separately reviewed capability explicitly supports them.
- [ ] **FR-WAS-06:** The system must run adversarial guest and host-interface tests covering memory/CPU exhaustion, malformed artifacts, cross-tenant handles, denial of service and worker recovery, and retain security-review evidence.

### JOB. Long-running analysis and recovery

- [ ] **FR-JOB-01:** The system must provide asynchronous job submission/status/progress APIs through Falcone with tenant-scoped cancellation and configurable duration limits separate from HTTP connection deadlines.
- [ ] **FR-JOB-02:** The system must persist explicit application checkpoints in SeaweedFS with pinned revision, schema version, input digest and fenced attempt identity; never advertise transparent arbitrary-memory restoration.
- [ ] **FR-JOB-03:** The system must resume checkpoint-aware jobs after worker/pod interruption using Temporal coordination and a single retry owner, with documented restart behavior for jobs without checkpoints.
- [ ] **FR-JOB-04:** The system must propagate cancellation to guest computation and blocking host calls, record acknowledged/uncertain states and enforce profile-specific termination budgets without claiming side-effect rollback.
- [ ] **FR-JOB-05:** The system must reserve separate interactive and long-running capacity within Falcone quotas, enforce fair admission and include checkpoint/drain state in worker scale-down decisions.
- [ ] **FR-JOB-06:** The system must benchmark representative hours-long, memory-intensive analysis and failure recovery, publishing wall time, CPU, memory, checkpoint overhead and restored progress on the supported runtime profile.

### OCI. OCI image execution mode

- [ ] **FR-OCI-01:** The system must let only the platform superadmin authorize OCI mode per tenant for none, all or selected projects (workspaces); enforce it at deploy, promotion and every admission path, audit each change with the acknowledged residual-risk version, and drain OCI work when authorization is revoked.
- [ ] **FR-OCI-02:** The system must accept an explicit per-function runtime.mode (wasm by default, or oci) recorded on the immutable revision, define a versioned OCI function container contract, and never switch a function between modes automatically.
- [ ] **FR-OCI-03:** The system must accept only digest-pinned OCI function images assembled or built FROM a visible catalog base, or BYO images (`base.kind: byo`) in superadmin-flagged projects with a valid signature and an advisory scan record; verify and copy each image into the platform intake mirror before any pod is rendered, without executing image content.
- [ ] **FR-OCI-04:** The system must serve OCI invocations through a trusted shim that implements the worker execution API over mutually authenticated TLS with a per-pod shim identity mounted only into the shim container, verifies the dispatcher identity, enforces concurrency and deadlines and reports honest cancellation outcomes; the dispatcher verifies the shim identity before sending any credential or request, and the shim never forwards the execution credential to the tenant container.
- [ ] **FR-OCI-05:** The system must run OCI functions in single-revision restricted pods (non-root, read-only root filesystem, no service-account token) materialized on demand in the tenant's bound OCI namespace, administrator-provided under default namespaceMode=preprovisioned; Accepted ADR 0011 describes dedicated namespaces in create after permission preflight and the release namespace in shared, subject to the operator amendment and R3 rollout approval. Shared OCI requires per-tenant superadmin enablement and verified tenant-scoped NetworkPolicy enforcement; the controller's write set is unchanged.
- [ ] **FR-OCI-06:** The system must give OCI functions storage, event, workflow, secret, logging and allow-listed HTTP access only through a localhost capability API and an egress gateway; keep the execution credential outside the tenant container and enforce OCI-namespace NetworkPolicies.
- [ ] **FR-OCI-07:** The system must qualify OCI isolation with adversarial evidence before enabling OCI for any tenant, and publish a versioned residual-risk register covering the shared kernel, restricted PSA, side channels and node patch ownership.
- [ ] **FR-OCI-08:** The system must charge OCI executions against the shared tenant execution slots plus OCI caps on materialized revisions, warm replicas, pod memory, image bytes and builds, and record per-mode usage and metrics.
- [ ] **FR-OCI-09:** The system must offer OCI as an explicit migration target only for workloads with an oci-candidate verdict in an authorized project, and keep workloads without a candidate verdict in either mode as Knative retirement blockers.
- [ ] **FR-OCI-10:** The system must provide CLI, console, starter and documentation support for OCI functions, including the warm-reuse notice, and show OCI options only in authorized projects.

### CAT. Superadmin base image catalog

- [ ] **FR-CAT-01:** The system must maintain one global, superadmin-managed base catalog of Wasm profile components and OCI base images with immutable name@version entries bound to one digest and visibility filtered by tenant and project.
- [ ] **FR-CAT-02:** The system must let the superadmin register existing bases by digest-pinned reference after artifact validation or image intake, record optional signature and provenance results, and enable a Wasm base only with profile qualification evidence.
- [ ] **FR-CAT-03:** The system must let the superadmin create bases from versioned recipes built by confined trusted builders with SBOM and provenance, and reject RUN steps in OCI recipes in v1.
- [ ] **FR-CAT-04:** The system must enforce the base lifecycle (draft, enabled, deprecated, retired, and disabled with optional invocation blocking) and default aliases without ever rebasing a revision automatically; in both runtime modes, a new function or a base switch needs an enabled base, a deprecated base stays eligible only for new revisions of functions already pinned to it until its window end, and retired and disabled bases are refused for new revisions.
- [ ] **FR-CAT-05:** The system must pin the selected or default catalog base by id@version and digest on every revision with `base.kind: catalog`, and reject absent, invisible, lifecycle-ineligible or unauthorized catalog bases at deploy, promotion and invoke; BYO revisions need no catalog base.
- [ ] **FR-CAT-06:** The system must audit every catalog and OCI authorization mutation, and provide superadmin console pages plus a tenant base selector filtered by visibility.


## 3. Baseline architecture — selected Option A

### Components

```mermaid
flowchart TD
    Clients["Clients / CLI / console"] --> Edge["Existing APISIX"]
    Edge --> API["Falcone API / authorization / quotas"]
    Edge --> WS["Trusted WebSocket gateway"]
    API -. "verify" .-> IAM["Keycloak"]
    API --> DB["PostgreSQL / functions / activations / outbox"]
    WS --> Events["Event routing / existing Kafka"]
    Events --> Dispatcher["Scoped execution dispatcher"]
    DB --> Controller["Namespaced pool controller"]
    Controller -. "standard resources only" .-> Pool["Tenant and profile worker pools"]
    Dispatcher --> Pool
    API --> Dispatcher
    Pool --> Guest["Fresh Wasmtime guest execution"]
    Guest --> Host["Versioned capability host"]
    Host --> Bindings["SeaweedFS / OpenBao / Falcone APIs"]
    Temporal["Temporal and existing workflow worker"] --> Dispatcher
    Host --> Temporal
    Controller -. "standard resources only" .-> OciPool["OCI revision pools (shim + function container)"]
    Dispatcher --> OciPool
    OciPool --> CapApi["Localhost capability API in the shim"]
    CapApi --> Bindings
    CapApi --> Egress["Per-namespace egress gateway"]
    API --> Catalog["Superadmin base catalog"]
    Catalog --> Intake["Image intake / assembler"]
    Intake --> Mirror["Digest-pinned platform intake mirror"]
    Mirror -. "pull by digest" .-> OciPool
```

OpenTelemetry instruments gateways, dispatch, guest lifecycle and host calls. Telemetry and results always carry verified tenant/workspace scope. The diagram does not imply guest access to raw infrastructure credentials. In OCI mode (ADR 0010) the execution credential stays in the trusted shim and never enters the tenant container.

### Technology and deployment model

| Component | Selected approach | Reason |
| --- | --- | --- |
| Public API | Existing Falcone Node.js services and shared schemas | Preserve existing interfaces, identity, quotas and console |
| Controller | Go service using standard Kubernetes APIs with namespaced RBAC | Reconcile worker Deployments/Services and use namespaced Leases; no custom resource installation |
| Messaging | Existing trusted WebSocket gateway plus Kafka/event routing | Separate connection/message throughput from execution load |
| Dispatch/admission | Falcone service with durable records, reservations and revision pinning | Preserve tenant fairness and accepted-work durability |
| Execution worker | Rust embedding a pinned Wasmtime engine | Direct control of sandbox configuration, imports, budgets and interruption |
| Function artifacts | Validated raw Wasm modules/components plus immutable manifests, **or**, in authorized projects, a digest-pinned OCI image derived from a catalog base | No unvetted tenant OCI image execution; OCI is an explicit, superadmin-authorized mode (ADR 0010) |
| Language profiles | Qualified JavaScript adapter first; separately qualify Go and Python | Preserve source semantics where possible without pretending a full Linux environment exists |
| Host interfaces | Versioned WIT/WASI interfaces and narrow Falcone capabilities | Explicit grants for storage, HTTP, events and workflows |
| Storage | Existing PostgreSQL for metadata; SeaweedFS for artifacts/checkpoints/payloads | Reuse established persistence and tenant policies |
| Workflow engine | Existing Temporal/workflow-worker | Durable orchestration, retry coordination and long-job recovery |
| Image delivery | Approved digest-pinned Falcone worker/compiler images in existing registry; tenant OCI function images only through the platform intake mirror | Kubelets pull trusted platform images and verified, mirrored function images by digest, never directly from tenant registries |
| OCI shim | Trusted falcone-serverless sidecar in each OCI revision pod | Serves the worker execution API over mutually authenticated TLS, with a per-pod identity mounted only into the shim container, and a localhost capability API; keeps the execution credential out of the tenant container |
| Egress gateway | Trusted per-namespace HTTP egress gateway for OCI namespaces | Credential-checked allow-listed egress with DNS-rebinding, redirect and metadata checks |
| Image intake / assembler | Trusted falcone-serverless components rendered as restricted pods | Verify, assemble and mirror function images by digest; never run tenant build hooks |
| Base catalog | Falcone-owned global catalog of Wasm profile components and OCI bases | Superadmin-managed bases with tenant/project visibility, immutable versions and audited lifecycle |
| Installation | Namespaced Helm resources under existing corporate admission policy | No operator, CRD, RuntimeClass or elevated SCC prerequisite |

Wasmtime/WASI capability isolation is the selected boundary for untrusted guest code. It requires a correctly secured host interface and patch policy; it is not a claim of zero residual risk. [S26–S27] OCI mode is a weaker boundary: tenant code shares the node kernel under restricted Pod Security only, and the superadmin accepts that residual risk when authorizing a project (ADR 0010).

### Deployment authority and permission boundary

Default `namespaceMode=preprovisioned` uses administrator-provided namespaces and explicitly granted namespaced permissions; namespace creation is not a prerequisite in that mode. [Accepted ADR 0011](adr/0011-namespace-placement-mode.md) describes `create` for dedicated pool/OCI namespaces only after preflight proves the deployment identity may create namespaces and namespaced RBAC; denial fails closed with an error naming `namespaceMode`. It also describes `shared` for pools, controller-rendered Deployments/Services, OCI revision pods and the egress gateway in the release namespace, without namespace creation. Rollout of these alternatives still requires second review, operator approval and the operator hard-rule amendment; they are not active permissions. No mode grants cluster-wide roles or changes admission configuration. The installer needs explicitly granted namespaced permissions, not merely the ability to create a Deployment. In shared mode, per-tenant/per-revision names and labels, namespaced RBAC, distinct service accounts and tenant-scoped NetworkPolicies are mandatory. Labels and NetworkPolicies become the tenant boundary (R3); credential and RLS checks remain mandatory.

Preflight must verify required verbs for Deployments, Services, ConfigMaps, scoped Secrets, Jobs if used for trusted compilation, Leases and existing permitted NetworkPolicies. If permissions or network enforcement are absent, report the unmet prerequisite; do not silently disable isolation. Guest worker service accounts have no automatic Kubernetes API token. Controller access is separate and namespaced.

Corporate-owned ingress/TLS, Kafka, storage, database, network policy enforcement and existing SCC remain prerequisites. An application-only design does not create those facilities or grant itself permissions.

PostgreSQL remains authoritative for Function/Revision/Trigger/Job records. API transactions and outbox events drive standard Kubernetes worker-pool resources and route/config generations. No CRDs are required. Users submit declarative manifests through Falcone's API; they do not edit worker resource projections directly.

The native pool scaler is the only writer of replica counts. Desired-state reconciliation must not reset those counts. KEDA/HPA is not a mandatory dependency; any future alternate scaler requires an exclusive ownership handoff.

The controller also renders single-revision OCI pools (ADR 0010): one Deployment and one headless Service per materialized OCI revision, with the same kinds and verbs and no Pod, Job or Secret writes. OCI namespace resolution is mode-aware under Accepted ADR 0011: administrator-provided per tenant in default `namespaceMode=preprovisioned`, dedicated per tenant in `create` after namespace/RBAC permission preflight, or the release namespace in `shared`. Shared OCI additionally requires a per-tenant superadmin enablement of “OCI in shared namespace” and verified tenant-scoped NetworkPolicy enforcement. In every mode, the platform intake mirror pull Secret must already be provisioned in the resolved namespace and NetworkPolicy enforcement verified; absent enforcement blocks OCI. Namespace/RBAC creation remains installer authority, never an expansion of the controller write set.

Test fixture placement depends only on `testNamespaceMode`, independently of `namespaceMode` (Accepted ADR 0011; #121 is the harness entry point). Default `create` creates and deletes `fs-test-<run>`; `shared` uses `FS_TEST_NAMESPACE`, per-run resource names and `falcone.test/run=<nonce>`, cleans up only by its run label and refuses to touch unlabelled or other-run resources. Thus `namespaceMode=shared` with `testNamespaceMode=create` still creates test namespaces, while `namespaceMode=create` or `preprovisioned` with `testNamespaceMode=shared` uses the configured existing test namespace. Off-limits namespaces remain unchanged.

### Runtime contract and artifact security

A revision records source/artifact digest, language profile, engine/WASI/interface versions, supported imports, resource budgets, granted capabilities and effective configuration. Functions deploy as definitions and artifacts; **10,000 deployed functions do not allocate 10,000 pods or Services**.

**Wasm mode.** Tenant code may be uploaded as raw Wasm or source supported by an approved toolchain. Source builds compose onto the selected catalog Wasm base, which must be eligible under the base lifecycle rule below, and the revision pins its component digest. Source conversion uses an approved dependency catalogue. Arbitrary native package install hooks, Dockerfiles, binaries and shell scripts are rejected. A package needing unsupported native libraries must be reported as unsupported rather than built/executed on the host.

Validate uploaded modules before bounded compilation. Accept no tenant-produced native precompiled cache: native engine caches are generated only by trusted platform components and keyed by artifact digest, engine, architecture and configuration. Cache provenance is distinct from the portability of raw Wasm.

Begin with tenant/profile-specific worker pools. Each invocation receives a fresh guest instance and scoped handles. Pool capacity and trusted compiled code can be reused, but dirty guest memory, credentials, temporary files and handles cannot. Use a fresh trusted execution process per invocation for the first qualified implementation where feasible; benchmark the startup cost before enabling more aggressive in-process multiplexing. Process separation supplements the Wasm boundary; it does not replace sandbox validation.

Guest code cannot launch native processes or arbitrary host plugins. The trusted worker may launch the fixed execution harness; that is platform behavior, not a guest capability.

**OCI mode (ADR 0010).** An OCI revision records the mode, `base` as a tagged union (`{kind: catalog, id@version, digest}` or `{kind: byo, image digest, signer identity, attestation ref}`), the function image digest, container contract version, shim version, resources, grants and concurrency. Changing mode or base creates a new revision. Images reach a revision by one of three paths:

- P1: the trusted image assembler layers source or a prebuilt artifact onto a catalog base by pure layer assembly, with no RUN step and no tenant hook.
- P2: the tenant's CI builds `FROM <catalog base digest>`; intake verifies that the bottom layers equal the base manifest's layers, by digest.
- P3 (BYO, `base.kind: byo`): any base, only in projects the superadmin flags. It requires a valid signature against keys registered for the project, a platform-mirror copy and an advisory scan record.

Catalog-membership validation and base lifecycle controls (deprecation, retirement) apply only to `kind: catalog`; no BYO revision requires a catalog base. In both modes, catalog base eligibility follows the lifecycle: a new function, or a function switching base, needs an `enabled` base visible to the project; a new revision of a function already pinned to a base may keep that base while it is `deprecated`, until `window_end`; `retired` and `disabled` bases are refused for new revisions, and `disabled` with `block_invocations` also denies invocations of pinned revisions (`base_disabled`).

Ancestry is provenance and patch tracking, not a security boundary: upper layers can overwrite any base file. The isolation boundary is the restricted single-revision pod. Every image is copied by digest into the platform intake mirror before a pod is rendered. An OCI container process serves many invocations of the same revision (warm reuse): memory, `/tmp` and globals can persist between them, but credentials do not, because each per-attempt handle is invalidated. This is documented and shown before deploy; the fresh-instance rule applies to Wasm mode.

### Host capability model

Provide explicit interfaces for:

- Authorized object read/write and checkpoint access through Falcone storage.
- Allow-listed outbound HTTP with size/deadline limits, redirect and DNS-rebinding checks.
- Event publication with verified tenant/workspace routing.
- Allow-listed workflow start/signal operations through Falcone.
- Structured logging, cancellation state and explicit progress/checkpoint reporting.

Authorization is derived from verified invocation context, never caller-chosen guest fields. Credentials remain in the host binding layer unless a specific reviewed API explicitly grants a narrow secret to guest code. Guest handles are execution-scoped and invalid after completion.

In OCI mode there are no WIT host calls. Containers in a pod share the network namespace, so the dispatcher→shim channel is mutually authenticated TLS. The shim's per-pod identity is a certificate or projected token mounted only into the shim container; the pod sets `automountServiceAccountToken: false`, and the tenant container has no such volume. The dispatcher verifies the shim identity before it sends any credential or request. The shim→container hop uses a separate localhost port and never carries the credential. The trusted shim offers the same capabilities as a localhost capability API (`ctr-oci-capability-api`) that mirrors `falcone:host` and calls the existing Falcone host APIs with the per-attempt execution credential. The container receives only an opaque per-attempt handle that is invalid after the attempt, and scope still comes from the credential, never from request fields. Allow-listed outbound HTTP goes through the shim to the per-namespace egress gateway, which applies the same DNS-rebinding, redirect and metadata/private-range checks; NetworkPolicy limits raw sockets to Falcone host APIs, the egress gateway and DNS. A secret reaches OCI code only as a reviewed narrow guest grant fetched per attempt, never through environment variables, files or the image.

Wasmtime fuel/epoch interruption applies to guest computation, not every blocking host call. Host network/storage operations require their own timeout, cancellation and memory limits; a bounded process termination fallback handles unresponsive executions. [S28]

### Synchronous invocation

1. APISIX/Falcone validates Keycloak identity, resolves tenant/workspace and checks capability/plan. For an OCI revision, admission also checks the project's OCI authorization and, for a `kind: catalog` base, the base state.
2. Admission reserves a slot and pins a function revision/profile. Required activation state is persisted.
3. Dispatch selects a pool by revision mode: the authorized tenant/profile pool for Wasm, or the single-revision OCI pool, which is materialized on first demand. If the pool is at zero, activate it and wait within the bounded request deadline.
4. Wasm: the trusted execution harness validates the artifact and creates a fresh Wasm instance with only approved imports and grants. OCI: the dispatcher and the shim authenticate each other over mutual TLS, and the dispatcher verifies the shim's per-pod identity before it sends the credential or request. The shim enforces concurrency and deadlines and forwards the attempt to the function container on a separate localhost port that never carries the credential.
5. Guest code runs; host operations independently enforce scope, budgets and cancellation.
6. Persist outcome, usage and result references; return the existing Falcone envelope for compatible profiles and release reservations.

An HTTP disconnect or timeout does not prove the execution had no external effect. Do not replay uncertain side-effecting calls blindly.

### WebSockets, event delivery and scale

Trusted gateways terminate WebSockets, maintain authentication/reconnect behavior and apply backpressure. Event routing can forward, filter or batch messages without invoking user code when existing semantics permit. Do not silently batch or drop messages if the public contract requires individual delivery.

Persist durable accepted invocations and outbox entries atomically. Kafka dispatch remains at-least-once; dedupe, fenced attempts, retries and DLQ/replay remain explicit. Durable message acceptance and transient WebSocket delivery are different promises and need separate API/benchmark definitions.

Scale connection gateways by connection/message load, dispatchers by backlog, and execution pools by admitted running/waiting work and profile capacity. A basic pool demand estimate is running plus admitted waiting executions divided by per-replica execution capacity, bounded by Falcone quotas and stabilization policy. Validate the control loop under burst/idle/failure tests. OCI pools scale per revision: the demand signal carries per-revision entries, idle revisions dematerialize after a TTL, and a per-tenant quota bounds materialized OCI revisions.

Track incoming messages, outgoing deliveries after fan-out, bytes, triggered invocations and open connections separately. For example, 1% of 100,000 messages triggering one function means 1,000 invocations/second. At a one-second average duration, that needs roughly 1,000 active execution slots; the provisional 400-slot quota would queue or reject excess work. Long jobs further consume capacity, so reserve interactive and analysis budgets.

### Long-running analysis

Expose asynchronous jobs independently of HTTP/WebSocket lifetimes. Require configurable maximum runtime, heartbeat/progress, cancellation, checkpoint retention and explicit restart/resume policy per profile.

Application checkpoints in SeaweedFS contain input digest, revision/profile, checkpoint schema and attempt-fencing identity. A retried worker resumes only from a compatible committed checkpoint. The platform does not promise transparent restoration of arbitrary guest memory.

Temporal coordinates accepted jobs through Activities and one retry owner. On retry, recover the existing accepted execution before launching another attempt. Preserve function revision pins and existing failure classification. Completed Activity history replay is not permission to re-execute external effects. [S3, S22]

Worker scale-down must account for running jobs, checkpoint opportunities and maximum drain periods. Define termination semantics for uncheckpointed jobs honestly: restart may repeat work and side effects.

In OCI mode the shim relays progress and checkpoints through the same execution-updates contract. Cancellation first asks the container over HTTP; the bounded kill fallback is a liveness-probe restart that the shim triggers by failing the probe, which needs no Pod-delete RBAC.

### Migration and compatibility

Retain public Falcone identifiers, APIs, URLs, schemas, ownership and activation history. Inventory JavaScript dependencies and actual function behavior, not only example request/response JSON. Python threading, process and OS interfaces can differ under Wasm; supported packages must be demonstrated. [S29]

Keep existing Knative only as an optional migration reference where already installed. A clean Option A installation must not install or depend on it. Unsupported old workloads stay explicitly blocked from cutover; they must be adapted or separately resolved before full retirement. ADR 0010 makes OCI the explicit compatibility path: classification records a verdict per mode, and a workload can target OCI only with an oci-candidate verdict in an OCI-authorized project. Nothing selects OCI automatically, and workloads without a candidate verdict in either mode still block Knative retirement. OCI mode is Falcone-owned and installs no Knative.

Trusted platform services such as Falcone's own MCP adapter can remain ordinary approved container services. This does not allow tenant-supplied arbitrary MCP server images. Only WebSockets are currently confirmed by the user; preserve any actually deployed MCP contracts found in discovery and leave speculative additional transports out of the initial mandatory scope.

### Proposed measurable objectives

Targets below are provisional engineering objectives, not measured results or contractual SLAs.

| Dimension | Qualification objective |
| --- | --- |
| Inventory | 20 tenants / 10,000 deployed definitions; measure metadata read/deploy latency and controller load independently from worker count |
| Messaging | 100,000 aggregate ingress messages/s under a declared payload, connection, fan-out, durability and trigger-fraction profile; separately report egress and function rates |
| Starting quotas | 20 concurrent executions per tenant, configurable entirely in Falcone; separate analysis and interactive reservations |
| Warm latency | p95 platform overhead ≤20 ms for a preloaded small function under an explicit invocation-rate test; exclude handler duration and report p99 |
| Cold latency | p95 ≤5 s for an agreed small cached-artifact function on available capacity; report Wasm compilation, language interpreter startup, pod start and image pull separately |
| Availability | Initial 99.9% monthly eligible-invocation availability; define dependency failures and quota/overload exclusions before any SLA |
| Single failure | No loss of durably acknowledged asynchronous work within the configured HA/retention envelope; duplicates remain possible |
| Service recovery | Proposed ≤60 s for a single service failure; measure separately from long-job progress recovery and WebSocket reconnection |
| Disaster recovery | Proposed RPO ≤5 min and RTO ≤60 min, conditional on actual backups/replication and a restore exercise |
| Long jobs | Hours-long representative analysis, bounded resource usage and recovery from the last committed application checkpoint; checkpoint interval/loss budget still to agree |
| Sandbox | Adversarial guest/host-boundary tests, independent review and actual restricted-OpenShift evidence before hostile-code production enablement |
| OCI cold latency | Provisional p95 ≤10 s with a node-cached image under 250 MB; report pod scheduling, image pull, container start, application init and contract probe separately; uncached pulls are reported, not targeted |
| OCI warm latency | Provisional p95 ≤25 ms platform overhead (dispatcher → shim → localhost), excluding handler duration |
| OCI isolation | Adversarial OCI suite (OCI-07) and preflight-confirmed NetworkPolicy enforcement before OCI is enabled for any tenant |

The 100,000-message target is not coupled to the warm-invocation latency target until a mixed traffic profile is agreed. No throughput benchmark has been run for this proposal.



## 4. Detailed backlog — GitHub-ready
16 epics and 58 implementation stories. Every story includes a title, user story, description, acceptance criteria, requirement links, dependencies, phase and effort. Option A supersedes earlier native-container assumptions; ADR 0010 adds the superadmin-authorized OCI mode (OCI) and base catalog (CAT) epics. Stable IDs are retained for traceability.

**Sizing:** S = contained change; M = several focused changes or integrations; L = substantial work with failure/security validation. These are relative estimates, not person-day commitments. No S-sized stories are used because these initial items are integration-level slices.

**Board:** Status = Backlog / Ready / In progress / Review / Validation / Done; group by parent epic; filter by area, phase and effort. M0–M3 are delivery phases, not deadlines. Parent/sub-issue hierarchy is imported natively; dependencies and requirements are included in issue bodies. The importer represents area, phase and effort as labels and does not create custom Project fields.

### API. API and developer experience

- **Epic EP-API: Expose the new runtime through Falcone's existing product contracts.**

- **API-01 — Preserve the Functions API and add CLI operations** · L · M1
  - **Story:** As a developer, I want to manage functions through familiar API and CLI operations, so that existing applications continue to work.
  - **Description:** Implement the public facade and CLI against the same validated contracts. Risk: R3 (tenant isolation).
  - **Acceptance:** (1) Legacy API fixtures pass against both backends, including status and error envelopes. (2) CLI deploy/invoke/logs/rollback/delete works with Keycloak credentials; replaying a mutation idempotency key creates no duplicate resource. (3) Unauthorized requests and tenant-scoped reads retain the baseline behavior. (4) The Functions API contract v1 defines `runtime.mode` (default `wasm`), the `base` field and the `oci_not_authorized` and base error codes; the legacy fixtures pass in Wasm mode (ADR 0010).
  - **Requirements:** FR-API-01, FR-API-02. **Dependencies:** MIG-01, SEC-01.

- **API-02 — Deliver language starters and declarative manifests** · M · M2
  - **Story:** As a function author, I want to develop locally and deploy a validated manifest, so that development matches hosted execution.
  - **Description:** Add three starter templates, local runner and schema validation. Manifest v1 adds `runtime.mode` (default `wasm`), `base` as a tagged union: `{kind: catalog, id@version, digest}` or `{kind: byo, image digest, signer identity, attestation ref}`, `image` and an OCI resources block, and an OCI starter is added (ADR 0010). Risk: R3 (host capabilities).
  - **Acceptance:** (1) Go, Python and JavaScript examples pass the same inputs locally and remotely. (2) Unsupported runtime versions, reserved environment variables and malformed manifests fail before a build or runtime write. (3) Manifest schema versions and defaults are documented and covered by compatibility fixtures. (4) The manifest schema v1 defines `runtime.mode` and the `base` tagged union with fixtures for both kinds; OCI-mode validation (authorization, catalog and BYO checks) is delivered by OCI-02 (ADR 0010).
  - **Requirements:** FR-API-03, FR-API-04. **Dependencies:** RUN-01, DEP-01.

- **API-03 — Integrate runtime status, console and SDK schemas** · M · M2
  - **Story:** As a workspace administrator, I want to see reliable status and typed APIs, so that I can diagnose deployments without learning Kubernetes.
  - **Description:** Wire the existing console and OpenAPI generation to the backend-neutral status model. Risk: R3 (tenant isolation).
  - **Acceptance:** (1) Console distinguishes disabled, unavailable, deploying, active and deletion_pending; retry controls remain keyboard accessible. (2) Tenant views contain no other-tenant resources or cluster internals; operator views are separately authorized. (3) Generated client tests cover successful and unavailable runtime responses. (4) The status model and console show the runtime mode and the OCI and base failure reasons; superadmin OCI authorization and catalog pages are delivered with CAT-06 (ADR 0010).
  - **Requirements:** FR-API-05, FR-API-06. **Dependencies:** API-01, OBS-01.

### RUN. Runtime and execution

- **Epic EP-RUN: Run qualified Wasm functions while preserving supported legacy source contracts; Wasm by default, OCI where authorized.**

- **RUN-01 — Implement Wasmtime execution and qualified source adapters** · L · M1
  - **Story:** As a function author, I want to run supported Wasm and legacy source functions, so that I can migrate without rewriting handlers.
  - **Description:** Build the Rust/Wasmtime worker, beginning with JavaScript contract qualification; Go and Python profiles are enabled only after matrix tests. Risk: R3 (sandbox, host capabilities).
  - **Acceptance:** (1) In mode `wasm`, validated Wasm artifacts execute in fresh instances using the declared imports; native executable and arbitrary tenant-image submissions are rejected, and the Wasm worker refuses any revision whose runtime mode is not `wasm` (ADR 0010). (2) Qualified main(params), status/result/logs and invocation-identity fixtures pass unchanged; incompatible legacy functions are explicitly blocked from cutover. (3) Each activation records function revision, runtime mode, raw artifact digest, base digest when the revision pins one, Wasmtime version, language profile and interface version.
  - **Requirements:** FR-RUN-01, FR-RUN-02. **Dependencies:** MIG-01, SEC-02, WAS-03.

- **RUN-02 — Enforce guest and host limits with safe draining** · L · M1
  - **Story:** As an operator, I want to bound each execution and stop it predictably, so that one function cannot exhaust the service.
  - **Description:** Enforce deadlines and memory budgets in both guest execution and Falcone host operations. Risk: R3 (sandbox, host capabilities).
  - **Acceptance:** (1) Infinite loops, guest allocation, host-buffer growth, compilation exhaustion and oversized I/O hit documented limits without exhausting the worker pool. (2) Guest interruption and blocked storage/network host-call cancellation are independently tested; kill fallback replaces only the affected execution process. (3) Draining rejects new work and completes, checkpoints or cancels admitted executions within profile-specific budgets.
  - **Requirements:** FR-RUN-03, FR-RUN-04. **Dependencies:** RUN-01, INV-01.

- **RUN-03 — Enforce fresh instances and runtime support policy** · M · M2
  - **Story:** As an operator, I want to manage supported runtimes and storage bindings, so that upgrades do not change function behavior unexpectedly.
  - **Description:** Keep compiled-code caching separate from tenant execution state. Risk: R3 (host capabilities).
  - **Acceptance:** (1) In mode `wasm`, sequential executions cannot observe prior guest memory, handles, environment or temporary files, including after traps and cancellation. (2) Artifact caches use digest, engine, architecture and interface-version keys; user-supplied native precompiled caches are rejected. (3) Unsupported package/profile versions fail deployment with actionable diagnostics; a supported profile comes only from a catalog Wasm entry that is eligible under the ADR 0010 D8 lifecycle rule (`enabled` for a new function or a base switch; `enabled`, or `deprecated` until `window_end`, for a new revision of a function already pinned to that base; `retired` and `disabled` refused for new revisions); upgrades preserve immutable revision references.
  - **Requirements:** FR-RUN-05, FR-RUN-06. **Dependencies:** RUN-01, STO-01.

### DEP. Build, revisions and deployment

- **Epic EP-DEP: Produce traceable artifacts and safe, repeatable rollouts.**

- **DEP-01 — Build and verify Wasm function artifacts** · L · M2
  - **Story:** As a developer, I want to build trusted Wasm artifacts from source or submit a module, so that deployments are reproducible and traceable.
  - **Description:** Use approved language toolchains and package catalogues; isolate compilation and prohibit native dependency hooks. Source builds compose onto the selected catalog Wasm base; for OCI, the trusted image assembler layers source or a prebuilt artifact onto a catalog base without RUN steps or tenant hooks (ADR 0010). Risk: R3 (sandbox, host capabilities, secrets, tenant isolation).
  - **Acceptance:** (1) Supported source and uploaded raw Wasm produce validated digest-pinned revisions with SBOM, provenance and declared imports. (2) Disallowed imports/features, invalid signatures, native executables and submitted native compiled caches are rejected before worker execution. (3) Build hooks cannot execute arbitrary native code; malicious compilation inputs are bounded and cannot read another tenant’s artifacts or credentials. (4) A build composes only onto its pinned catalog base digest and fails with `base_mismatch` otherwise; the OCI assembler path runs no tenant hook.
  - **Requirements:** FR-DEP-01, FR-DEP-02. **Dependencies:** WAS-03, SEC-02, STO-01, TEN-02.

- **DEP-02 — Implement revisions, weighted rollout and rollback** · L · M2
  - **Story:** As a developer, I want to release gradually and roll back, so that faulty code affects fewer users.
  - **Description:** Add immutable revisions and route-generation updates. Risk: R2 (no plan task changes the sandbox, host capabilities, egress, secrets or tenant isolation).
  - **Acceptance:** (1) A bad readiness probe never receives production traffic. (2) Configured canary weights meet the test tolerance over the agreed request sample; rollback restores the previous target without rebuilding. (3) Concurrent updates use compare-and-swap; requests already admitted retain their revision. (4) Revision identity includes runtime mode and base and image digests; promotion re-checks OCI authorization and base state; a cross-mode canary runs only when the rollout explicitly opts in (ADR 0010).
  - **Requirements:** FR-DEP-03, FR-DEP-04. **Dependencies:** RUN-01, INV-01.

- **DEP-03 — Reconcile worker pools and collect artifacts safely** · L · M2
  - **Story:** As an operator, I want to recover drift and remove unused artifacts, so that restarts do not leak or destroy workloads.
  - **Description:** Reconcile standard namespaced workloads and reference-aware artifact/cache cleanup. Risk: R3 (host capabilities, tenant isolation).
  - **Acceptance:** (1) A controller restart converges to one owned worker-pool configuration and routing generation; stale updates cannot overwrite newer state. (2) A 10,000-deployed-function inventory does not create 10,000 pods or per-function Services; worker count follows admitted compute demand. (3) GC retains active, workflow-pinned, in-flight and rollback-protected artifacts and removes only expired owned data. (4) Single-revision OCI pools are materialized only within the per-tenant bound; GC also covers intake-mirror images and catalog bases after reference checks (ADR 0010).
  - **Requirements:** FR-DEP-05, FR-DEP-06. **Dependencies:** DEP-02, OPS-01.

### INV. Invocation, routing and autoscaling

- **Epic EP-INV: Provide controlled synchronous and asynchronous execution, including scale from zero.**

- **INV-01 — Implement authenticated sync and durable async invocation** · L · M1
  - **Story:** As an application developer, I want to invoke synchronously or submit durable work, so that I can choose the right response model.
  - **Description:** Build router admission, activation records and transactional outbox submission. Risk: R3 (tenant isolation).
  - **Acceptance:** (1) Sync calls return the existing envelope; async returns 202 plus an activation ID only after the acceptance transaction commits. (2) Crashes before/after outbox publication do not lose accepted work; duplicate delivery maps to one logical invocation. (3) A gateway timeout after dispatch is recorded as uncertain where necessary and never automatically retried as a safe read. (4) The dispatch contract carries the pinned revision's runtime mode; until EP-OCI delivers OCI pools, admission refuses a revision that is not in mode `wasm` with a non-retryable error (ADR 0010).
  - **Requirements:** FR-INV-01, FR-INV-02. **Dependencies:** API-01, STO-03, SEC-01.

- **INV-02 — Implement bounded admission and worker-pool activation** · L · M2
  - **Story:** As an operator, I want to scale with demand within quotas, so that bursts remain controlled and idle functions release capacity.
  - **Description:** Scale tenant/profile pools from execution demand while keeping deployment metadata independent of worker capacity. Risk: R2 (no plan task changes the sandbox, host capabilities, egress, secrets or tenant isolation).
  - **Acceptance:** (1) A zero-replica tenant/profile pool wakes for admitted execution and returns bounded cold-start or queue errors. (2) Multi-router tests enforce configurable tenant quotas; the initial default is 20 simultaneous executions per tenant, not a throughput guarantee. (3) The pool scaler never follows raw WebSocket message count directly; stale metrics and long-running jobs cannot cause unsafe scale-down. (4) The demand signal and scaler include per-revision OCI pools, which scale to zero when idle (ADR 0010).
  - **Requirements:** FR-INV-03, FR-INV-04. **Dependencies:** INV-01, TEN-02, OBS-02.

- **INV-03 — Preserve WebSocket behavior and benchmark message routing** · L · M3
  - **Story:** As a MCP application developer, I want to keep streaming and sessions working, so that runtime migration does not interrupt tools.
  - **Description:** Separate trusted WebSocket gateway capacity from Wasm execution and qualify an explicit traffic profile. Risk: R2 (no plan task changes the sandbox, host capabilities, egress, secrets or tenant isolation).
  - **Acceptance:** (1) Compatibility tests cover authentication, ordering, backpressure, reconnect and draining; existing deployed MCP behavior, if found, is inventoried separately. (2) Benchmark 100,000 aggregate ingress messages/second with declared payload size, open connections, fan-out, durability and function-trigger fraction; distinguish input from output deliveries. (3) Connection counts, message rates, invocation rates and running guests are separate metrics and quotas; no 100,000-invocations/second claim is made.
  - **Requirements:** FR-INV-05, FR-INV-06. **Dependencies:** MIG-01, RUN-02, INV-02.

### EVT. Events and triggers

- **Epic EP-EVT: Deliver tenant-scoped events with explicit retry and recovery semantics.**

- **EVT-01 — Implement CloudEvents adapters and trigger management** · L · M2
  - **Story:** As an integration developer, I want to connect authorized event sources to functions, so that I can react to platform changes.
  - **Description:** Add Kafka, storage and webhook adapters plus trigger lifecycle APIs. Risk: R3 (host capabilities).
  - **Acceptance:** (1) Fixtures for each source produce the versioned envelope and validated resource ownership. (2) Forged payload tenant IDs cannot change the destination scope. (3) Filters, pause/resume and deletion operate without silently discarding accepted deliveries.
  - **Requirements:** FR-EVT-01, FR-EVT-02. **Dependencies:** SEC-01, INV-01, STO-02.

- **EVT-02 — Implement retry, deduplication, DLQ and replay** · L · M2
  - **Story:** As an operator, I want to recover failed deliveries visibly, so that events are not silently lost.
  - **Description:** Build delivery-attempt records, fenced claims and explicit redrive controls. Risk: R3 (tenant isolation).
  - **Acceptance:** (1) Crash-after-handler-success/before-ack causes a traceable possible duplicate, never an exactly-once claim. (2) Retries stop at the configured limit, persist to DLQ and can be replayed by an authorized operator. (3) Dedupe retention, replay identity and ordering modes are documented and tested, including poison messages.
  - **Requirements:** FR-EVT-03, FR-EVT-04. **Dependencies:** EVT-01, STO-03.

- **EVT-03 — Integrate schedules and ordered dispatch** · M · M2
  - **Story:** As a workspace administrator, I want to schedule work and choose event ordering, so that automation runs predictably.
  - **Description:** Reuse Falcone scheduling ownership and partition-aware delivery. Risk: R2 (no plan task changes the sandbox, host capabilities, egress, secrets or tenant isolation).
  - **Acceptance:** (1) Cron fixtures cover DST transitions, timezones, missed executions and overlap policies. (2) Two scheduler instances cannot create two logical occurrences for one schedule timestamp. (3) Strict per-key mode blocks later events behind failure; relaxed mode documents and tests reordering.
  - **Requirements:** FR-EVT-05, FR-EVT-06. **Dependencies:** EVT-02, TEN-02.

### WF. Temporal integration

- **Epic EP-WF: Retain durable workflow semantics while changing the compute backend.**

- **WF-01 — Preserve functions.invoke and retry identity** · L · M2
  - **Story:** As a workflow author, I want to invoke the new backend from existing Flows, so that my workflows retain their behavior.
  - **Description:** Adapt the existing worker activity, preserving workspace binding and failure classification. Risk: R3 (tenant isolation).
  - **Acceptance:** (1) Existing functions.invoke tests pass, including non-retryable missing-function/runtime errors and retryable timeouts. (2) A task input cannot override the credential-bound workspace. (3) Retries share a logical key while recording separate attempt IDs and the same pinned revision. (4) `functions.invoke` covers both runtime modes; `oci_not_authorized` and `base_disabled` map to non-retryable errors (ADR 0010).
  - **Requirements:** FR-WF-01, FR-WF-02. **Dependencies:** MIG-01, INV-01.

- **WF-02 — Coordinate revision pins, retries and cancellation** · L · M2
  - **Story:** As a workflow author, I want to resume safely after failures, so that recovery does not unexpectedly execute new code.
  - **Description:** Define retry ownership and preserve activation/pin state across worker restarts. Risk: R2 (no plan task changes the sandbox, host capabilities, egress, secrets or tenant isolation).
  - **Acceptance:** (1) Deploying a new function during an Activity retry does not change the pinned revision. (2) A worker crash resumes/polls the same accepted invocation when its result is unresolved; a known retryable failure creates a new recorded attempt. (3) Heartbeat and cancellation tests expose requested, acknowledged or uncertain outcomes and avoid nested retry multiplication.
  - **Requirements:** FR-WF-03, FR-WF-04. **Dependencies:** WF-01, DEP-02, RUN-02.

- **WF-03 — Expose scoped workflow start and signal operations** · M · M2
  - **Story:** As a function author, I want to start and signal workflows safely, so that functions can participate in durable processes.
  - **Description:** Add a narrow capability-controlled bridge and large-payload references. Risk: R3 (host capabilities, tenant isolation).
  - **Acceptance:** (1) Allowed workflow starts/signals succeed with idempotent request IDs and audit records. (2) Cross-tenant targets and unlisted workflow types are denied even with forged input fields. (3) Large payloads use authorized SeaweedFS object references; Temporal histories contain no raw credentials.
  - **Requirements:** FR-WF-05, FR-WF-06. **Dependencies:** WF-01, STO-01, SEC-03.

### SEC. Identity and security

- **Epic EP-SEC: Enforce verified identities and safe execution at every trust boundary; Wasm by default, OCI where authorized.**

- **SEC-01 — Implement Keycloak verification and Falcone authorization** · L · M1
  - **Story:** As a security engineer, I want to verify identity and scope at each API boundary, so that tenant claims cannot be forged.
  - **Description:** Reuse Falcone's realm/trust policy and test public and internal entry points. Risk: R3 (secrets, tenant isolation).
  - **Acceptance:** (1) Expired, wrong-audience, wrong-issuer, forged and untrusted-realm tokens are rejected. (2) JWKS rotation succeeds within the configured cache budget; unknown keys fail closed during outages. (3) Tenant, workspace, platform-reader and mutation persona tests match the existing authorization matrix. (4) The platform service identity contract v1 reserves the `oci-shim`, `image-intake` and `image-assembler` callee audiences and the dispatcher→shim mutual-TLS identity (ADR 0010).
  - **Requirements:** FR-SEC-01, FR-SEC-02. **Dependencies:** MIG-01.

- **SEC-02 — Qualify restricted pods and the application sandbox** · L · M1
  - **Story:** As a platform operator, I want to run untrusted modules under the approved application sandbox, so that tenant code cannot obtain host privileges.
  - **Description:** Run trusted worker processes and untrusted Wasm guests under existing corporate policy. Risk: R3 (sandbox, host capabilities).
  - **Acceptance:** (1) In default namespaceMode=preprovisioned, installation and workload startup pass with the actual namespace identity and existing SCC, without cluster writes, host privileges or custom RuntimeClass; create/shared placement is governed by ADR 0011. (2) Guests cannot invoke native processes, load native libraries or obtain Kubernetes/admin credentials; dangerous imports are rejected. (3) Sandbox failure or unsupported features stop execution; there is no native fallback. Worker and compilation isolation tests are documented.
  - **Requirements:** FR-SEC-03, FR-SEC-04. **Dependencies:** WAS-02, OPS-01.

- **SEC-03 — Secure host capabilities, egress and audit** · L · M1
  - **Story:** As a security engineer, I want to bind internal requests to a scoped identity, so that a compromised function cannot impersonate another tenant.
  - **Description:** Implement the privileged host-operation boundary separately from guest code. Risk: R3 (host capabilities, egress, secrets, tenant isolation).
  - **Acceptance:** (1) Every host call verifies execution scope and capability grants; guessed object IDs, credential handles and forged tenant fields fail. (2) Metadata endpoints, DNS rebinding, redirects to forbidden destinations and unauthorized cross-tenant traffic are denied, including through host-call proxies. (3) Secrets and signing keys are absent from guest artifacts and telemetry; exposing a secret to guest code requires an explicit narrow grant. (4) The execution credential and audit event contracts v1 allow the trusted OCI shim as a credential holder, never the tenant container, and define the `oci_authorization.*` and `catalog.*` audit events (ADR 0010).
  - **Requirements:** FR-SEC-05, FR-SEC-06. **Dependencies:** SEC-01, SEC-02.

### TEN. Tenancy, plans and quotas

- **Epic EP-TEN: Preserve Falcone's tenant contract and bound noisy-neighbor effects.**

- **TEN-01 — Implement tenant scoping and namespace bindings** · L · M1
  - **Story:** As a tenant administrator, I want to have resources isolated by tenant and workspace, so that other customers cannot observe or alter them.
  - **Description:** Use existing identity scope plus an operator-owned namespace resolver. Risk: R3 (host capabilities, tenant isolation).
  - **Acceptance:** (1) SQL/RLS, API, log, artifact and activation tests reject adjacent tenants and unauthorized sibling workspaces. (2) Same function name in two tenants produces distinct owned runtime resources. (3) Unknown namespace mappings fail before Kubernetes writes; tenant input cannot choose a privileged namespace. (4) The namespace-bindings contract v1 includes an optional `oci_namespace` per tenant (ADR 0010): administrator-provided in default namespaceMode=preprovisioned; Accepted ADR 0011 describes dedicated namespaces in create after permission preflight and release-namespace bindings in shared, subject to the operator amendment and R3 rollout approval. Shared OCI additionally requires per-tenant superadmin enablement and verified NetworkPolicy enforcement.
  - **Requirements:** FR-TEN-01, FR-TEN-02. **Dependencies:** SEC-01, OPS-01.

- **TEN-02 — Enforce Falcone-owned quotas and workload fairness** · L · M1
  - **Story:** As a platform operator, I want to apply plans and fair capacity limits, so that one tenant cannot consume all shared capacity.
  - **Description:** Keep all accounting and policy in Falcone; distinguish messages/connections, executions and long-running work. Risk: R2 (no plan task changes the sandbox, host capabilities, egress, secrets or tenant isolation).
  - **Acceptance:** (1) Multi-router tests never exceed the configured aggregate tenant limits, initially 20 execution slots per tenant where enabled. (2) Interactive and batch budgets prevent analysis jobs starving interactive traffic; expired reservations recover with fenced ownership. (3) Failed attempts, retries, cold starts and warm idle capacity generate distinct usage records; Falcone configuration alone determines charging.
  - **Requirements:** FR-TEN-03, FR-TEN-04. **Dependencies:** TEN-01, STO-03.

- **TEN-03 — Implement suspension and durable scoped cleanup** · L · M3
  - **Story:** As a tenant administrator, I want to suspend and remove workloads reliably, so that disabled tenants stop consuming services.
  - **Description:** Unify suspension, deletion and tenant teardown around durable owner-checked cleanup. Risk: R3 (tenant isolation).
  - **Acceptance:** (1) Suspension stops new admissions and trigger delivery; queued/in-flight policy is applied and audited. (2) Deletion during an outage returns deletion_pending and resumes idempotently after recovery. (3) UID/resourceVersion/ownership mismatch retains the resource and raises a repair condition; completion requires verified absence. (4) Suspension and deletion cover OCI pools and intake-mirror images; revoking OCI authorization drains through the same in-flight and queued-work policy (ADR 0010).
  - **Requirements:** FR-TEN-05, FR-TEN-06. **Dependencies:** TEN-01, DEP-03, EVT-02.

### STO. SeaweedFS, secrets and metadata

- **Epic EP-STO: Reuse Falcone storage and secrets without introducing new identity or persistence silos.**

- **STO-01 — Implement Wasm artifact, checkpoint and scoped S3 storage** · M · M1
  - **Story:** As a function author, I want to access artifacts and application objects securely, so that my function can use Falcone storage.
  - **Description:** Reuse Falcone storage through capability-controlled host operations. Risk: R3 (host capabilities, secrets, tenant isolation).
  - **Acceptance:** (1) Required upload/download, multipart, signed-URL and checksum cases pass against both pinned SeaweedFS releases. (2) Guessed keys, path traversal and signed-URL requests cannot cross tenant/workspace boundaries; guest access uses narrow grants. (3) Retention preserves workflow-pinned artifacts, live payloads and resumable checkpoints; current notifications are preserved and regression-tested. (4) The host storage API contract v1 adds the `catalog-base` retention class and accepts the execution credential from any verified holder, including the trusted OCI shim (ADR 0010).
  - **Requirements:** FR-STO-01, FR-STO-02. **Dependencies:** SEC-01, TEN-01.

- **STO-02 — Bridge storage events and rotate OpenBao secrets** · L · M2
  - **Story:** As an integration developer, I want to receive object events and use rotating secrets, so that storage automation remains secure.
  - **Description:** Qualify the actual SeaweedFS notification/feed path and preserve OpenBao references. Risk: R3 (host capabilities, secrets, tenant isolation).
  - **Acceptance:** (1) PUT, multipart completion and DELETE through direct S3 clients produce normalized events according to the pinned-version contract. (2) Disconnect/reconnect tests establish replay checkpoints and any source-side loss window; unsupported guarantees block release of the corresponding trigger. (3) Secret rotation updates only authorized consumers and never writes values into revision manifests, telemetry or build outputs. (4) In OCI mode, a secret reaches code only as an explicit narrow guest grant fetched per attempt, never through environment variables, files or the image (ADR 0010).
  - **Requirements:** FR-STO-03, FR-STO-04. **Dependencies:** STO-01, SEC-03.

- **STO-03 — Implement durable invocation metadata and outbox recovery** · L · M1
  - **Story:** As an operator, I want to persist accepted work and recover dispatcher failures, so that acknowledged submissions survive restarts.
  - **Description:** Add scoped PostgreSQL schemas, outbox/inbox processing and fenced leases. Risk: R3 (tenant isolation).
  - **Acceptance:** (1) A transaction atomically persists activation plus outbox before returning accepted. (2) Crash tests at commit/publish/consume/complete boundaries recover pending work and reconcile duplicates. (3) A stale attempt cannot overwrite a newer terminal record; no guarantee of exactly-once arbitrary external side effects is advertised.
  - **Requirements:** FR-STO-05, FR-STO-06. **Dependencies:** MIG-01, OPS-01.

### OBS. Observability, audit and metering

- **Epic EP-OBS: Make failures, performance and consumption visible without leaking tenant data.**

- **OBS-01 — Add tenant-scoped logs and distributed tracing** · M · M1
  - **Story:** As a developer, I want to trace one invocation across services, so that I can locate failures quickly.
  - **Description:** Instrument runtime and Falcone adapters with OpenTelemetry and structured logs. Risk: R3 (tenant isolation).
  - **Acceptance:** (1) An HTTP and a Temporal-triggered invocation can each be followed across their full path. (2) Log/result/trace queries reject cross-tenant and unauthorized sibling-workspace access. (3) Tokens, secret values and request bodies are excluded or redacted according to policy. (4) The telemetry context contract v1 carries `runtime_mode` and base and image digests (ADR 0010).
  - **Requirements:** FR-OBS-01, FR-OBS-02. **Dependencies:** INV-01, SEC-01.

- **OBS-02 — Deliver metrics, dashboards and operational alerts** · M · M2
  - **Story:** As a SRE, I want to observe load and scaling decisions, so that I can diagnose saturation before an outage.
  - **Description:** Define bounded metrics and dashboards for the complete execution path. Risk: R3 (sandbox).
  - **Acceptance:** (1) Load tests populate warm, cold-start, queue and execution histograms separately. (2) Injected broker lag, stale metrics, unavailable runtime and failed cleanup trigger the expected alerts. (3) Metric-cardinality tests reject function-supplied labels and demonstrate a bounded series budget. (4) Metrics and dashboards split by runtime mode, including an OCI cold-start breakdown of image pull, pod start, init and contract probe (ADR 0010).
  - **Requirements:** FR-OBS-03, FR-OBS-04. **Dependencies:** OBS-01.

- **OBS-03 — Add auditable metering and retention controls** · M · M2
  - **Story:** As a platform administrator, I want to reconcile usage with execution records, so that quotas and future billing use explainable data.
  - **Description:** Implement idempotent usage events, audit queries and retention jobs. Risk: R3 (tenant isolation).
  - **Acceptance:** (1) Duplicate delivery does not double-count the same billable attempt; retry charging policy is explicit. (2) Usage totals reconcile with recorded execution attempts within the agreed lag budget. (3) Retention deletes expired telemetry without deleting active workflow or legal-hold references where configured. (4) Usage events carry the runtime mode; audit queries cover catalog and OCI authorization events (ADR 0010).
  - **Requirements:** FR-OBS-05, FR-OBS-06. **Dependencies:** OBS-01, STO-03, TEN-02.

### OPS. Operations, CI/CD and resilience

- **Epic EP-OPS: Ship and operate a supported, reproducible serverless subsystem.**

- **OPS-01 — Package namespace-only installation and resource ownership** · L · M1
  - **Story:** As a platform installer, I want to deploy a supported runtime reproducibly, so that cluster setup is inspectable and repeatable.
  - **Description:** Deploy APIs, controller and trusted worker pools with standard resources; reject missing prerequisites without requesting privilege escalation. Risk: R3 (sandbox, host capabilities, tenant isolation).
  - **Acceptance:** (1) Preflight using the deployment identity proves all required namespaced verbs. Default namespaceMode=preprovisioned and shared require no namespace creation or cluster-scoped rendered resources; create (ADR 0011, subject to the operator amendment and R3 rollout approval) additionally proves namespace and namespaced RBAC creation authority, failing closed with an error naming namespaceMode on denial. Its only cluster-scoped exception is dedicated Namespace creation; no mode permits privileged pods or custom SCC/RuntimeClass. (2) Actual pod startup and invocation pass on qualified Kubernetes lines and the supplied OpenShift patch release under its existing SCC. (3) Namespace prerequisites follow namespaceMode (preprovisioned by default; create/shared only after the operator amendment and R3 rollout approval under Accepted ADR 0011); gateway, storage, broker and RBAC prerequisites remain explicit, with no silent creation/adoption of cluster infrastructure. Test fixtures depend only on testNamespaceMode=create (default, `fs-test-<run>`) or shared (`FS_TEST_NAMESPACE`, per-run names and `falcone.test/run=<nonce>`, cleanup only by the owned run label and refusal to touch unlabelled or other-run resources); follow #121 and keep off-limits namespaces unchanged. (4) The serverless chart values contract v1 includes an OCI section that is disabled by default and renders no OCI resources (ADR 0010).
  - **Requirements:** FR-OPS-01, FR-OPS-02. **Dependencies:** MIG-01, WAS-01.

- **OPS-02 — Implement HA and the integration CI gate** · L · M2
  - **Story:** As a maintainer, I want to test the whole stack under failures, so that releases do not regress isolation or availability.
  - **Description:** Build real-stack CI plus multi-replica failover and GitOps workflows. Risk: R3 (tenant isolation).
  - **Acceptance:** (1) Node/controller/router/worker restarts recover according to documented sync/async guarantees. (2) CI exercises real Keycloak, SeaweedFS, Kafka and Temporal rather than mocks alone. (3) GitOps updates use the public authority path and cannot overwrite newer generations or bypass validation. (4) The real-stack CI suite includes an OCI function journey and an OCI authorization revocation test (ADR 0010).
  - **Requirements:** FR-OPS-03, FR-OPS-04. **Dependencies:** OPS-01, EVT-02, WF-02, TEN-02.

- **OPS-03 — Prove restore, upgrades and safe uninstall** · L · M3
  - **Story:** As a SRE, I want to recover and upgrade the platform, so that operations do not destroy accepted work or rollback options.
  - **Description:** Create tested recovery points, compatibility gates and operator runbooks. Risk: R3 (host capabilities, tenant isolation).
  - **Acceptance:** (1) Restore into a clean cluster reconciles resource ownership and pending invocations; measured RPO/RTO are recorded. (2) Upgrade/rollback tests cover data-format compatibility and refuse unsafe downgrades. (3) Default uninstall retains data; deletion requires explicit scoped intent and verified ownership. (4) Restore covers the base catalog, OCI authorization records and intake-mirror references (ADR 0010).
  - **Requirements:** FR-OPS-05, FR-OPS-06. **Dependencies:** OPS-02, TEN-03, DEP-03.

### MIG. Compatibility and Knative retirement

- **Epic EP-MIG: Replace the runtime incrementally with evidence of Falcone contract parity; Wasm by default, OCI where authorized.**

- **MIG-01 — Freeze Falcone contracts and accept Option A architecture** · M · M0
  - **Story:** As a maintainer, I want to record the exact replacement boundary, so that the implementation solves the real integration problem.
  - **Description:** Record the user-selected Wasm-default tenant execution direction, the superadmin-authorized OCI mode of ADR 0010, scope changes and migration blockers. Risk: R2 (no plan task changes the sandbox, host capabilities, egress, secrets or tenant isolation).
  - **Acceptance:** (1) Inventory marks each Falcone contract observed, source-defined or proposed and explicitly excludes raw Knative compatibility. (2) The ADR records 20 tenants, 10,000 deployed functions, 100,000 mostly WebSocket/event messages/second, no admin sandbox and Falcone-owned quotas. (3) Existing source dependencies are classified before any migration; API compatibility never disguises an unsupported execution environment. (4) The ADR scope note cites ADR 0010; classification records a Wasm verdict and an OCI verdict for each function.
  - **Requirements:** FR-MIG-01, FR-MIG-02. **Dependencies:** None.

- **MIG-02 — Implement backend selection and staged tenant migration** · L · M3
  - **Story:** As a platform operator, I want to move selected workloads between backends, so that I can evaluate the new runtime with a rollback path.
  - **Description:** Add backend drivers and generation-fenced route/trigger ownership. Risk: R2 (no plan task changes the sandbox, host capabilities, egress, secrets or tenant isolation).
  - **Acceptance:** (1) One public function identity can switch backend while keeping URLs, permissions and history. (2) At every cutover point only one backend owns production trigger delivery; non-idempotent traffic is never shadow-executed. (3) A failed canary can return to a retained compatible backend, with in-flight work drained or explicitly reconciled. (4) Backend selection targets OCI mode only by explicit choice through the guard, which requires an oci-candidate verdict and an OCI-authorized project (ADR 0010).
  - **Requirements:** FR-MIG-03, FR-MIG-04. **Dependencies:** DEP-02, WF-02, EVT-02, API-03.

- **MIG-03 — Certify Wasm compatibility and retire owned Knative resources** · L · M3
  - **Story:** As a platform owner, I want to remove Knative after proven compatibility, so that Falcone no longer requires its serving stack.
  - **Description:** Qualify actual existing workloads and retire only after all consumers have a supported execution path. Risk: R3 (tenant isolation).
  - **Acceptance:** (1) All mandatory public contracts and all in-scope migrated function/package cases pass; unsupported workloads remain explicit cutover blockers. (2) WebSockets, guest isolation, long-job checkpoint/recovery and any deployed MCP journeys pass; expected SLOs are measured rather than assumed. (3) No live workload or workflow pin depends on Knative; only proven-owned removable resources are retired through the authorized lifecycle path. (4) Certification and retirement count OCI-migrated workloads; workloads without a candidate verdict in either mode remain blockers (ADR 0010).
  - **Requirements:** FR-MIG-05, FR-MIG-06. **Dependencies:** MIG-02, INV-03, OPS-03, TEN-03, JOB-03, WAS-03.

### WAS. Wasm qualification and capability contract

- **Epic EP-WAS: Prove Option A works under actual corporate policy before building the complete platform.**

- **WAS-01 — Prove restricted OpenShift execution feasibility** · L · M0
  - **Story:** As a platform engineer, I want to validate Wasmtime under existing corporate policies, so that implementation starts from a deployable foundation.
  - **Description:** Build a minimal disposable qualification fixture and collect actual admission/runtime evidence. Risk: R3 (sandbox, tenant isolation).
  - **Acceptance:** (1) The deployment identity can install the fixture and execute/interrupt a guest without cluster-scoped writes, elevated SCC or custom RuntimeClass. (2) Compile/load behavior, executable-memory needs, arbitrary UID, read-only filesystem and temporary storage work under the actual policy; failures are reported without escalation or native fallback. (3) The exact OpenShift/Kubernetes versions and available namespaced permissions are recorded; absent access means unverified, not passed.
  - **Requirements:** FR-WAS-01, FR-WAS-02. **Dependencies:** MIG-01.

- **WAS-02 — Specify host capabilities and safe artifact loading** · L · M1
  - **Story:** As a security engineer, I want to control every sandbox escape hatch, so that tenant modules have only explicitly granted authority.
  - **Description:** Define the WIT/host API contract, invocation capability context and bounded compiler/cache path. Risk: R3 (sandbox, host capabilities, tenant isolation).
  - **Acceptance:** (1) Deny-by-default import tests and scope-bound storage/event/workflow/HTTP capability tests pass. (2) Tenant-provided native precompiled caches and arbitrary host-call implementations are rejected; only trusted locally generated caches are loaded. (3) Compiler CPU/memory limits, guest/host buffer limits and cancellation boundaries are part of the contract and have executable fixtures.
  - **Requirements:** FR-WAS-03, FR-WAS-04. **Dependencies:** WAS-01.

- **WAS-03 — Qualify language profiles and legacy compatibility** · L · M1
  - **Story:** As a function developer, I want to know which code and packages are supported, so that I can deploy without hidden compatibility changes.
  - **Description:** Test representative legacy JavaScript, real Python analysis dependencies and Go modules before enabling a profile. Risk: R3 (sandbox, host capabilities).
  - **Acceptance:** (1) Each enabled language has exact runtime/compiler/package versions, allowed interfaces, tested examples and unsupported-feature diagnostics. (2) The selected legacy JavaScript function and representative Python analysis pass expected results and dependency behavior; a failure blocks that profile or migration claim. (3) Host-interface fuzz/adversarial tests, fresh-instance leakage tests and a worker-crash recovery exercise are recorded before untrusted production enablement. (4) Each matrix entry has a stable profile id and published qualification evidence; WAS-03 consumes no base-catalog contract and is not gated on catalog work (the catalog later imports these profiles as `wasm-profile` entries). Compatibility diagnostics may name OCI as an explicit alternative but never select it (ADR 0010).
  - **Requirements:** FR-WAS-05, FR-WAS-06. **Dependencies:** WAS-02.

### JOB. Long-running analysis and recovery

- **Epic EP-JOB: Run extended analysis without losing progress or starving interactive traffic.**

- **JOB-01 — Implement asynchronous analysis jobs and checkpoint contracts** · L · M2
  - **Story:** As a data analyst, I want to save progress during extended executions, so that a pod restart does not require recomputing all completed work.
  - **Description:** Define job lifecycle, progress and explicit checkpoint/resume SDK contracts. Risk: R3 (host capabilities).
  - **Acceptance:** (1) Submission returns a durable job ID; progress and cancellation are scoped to the submitting tenant/workspace. (2) Checkpoint objects include revision/input/schema identifiers and cannot be written by a stale fenced attempt. (3) A sample analysis saves and reloads a checkpoint; an uncheckpointed job clearly reports that recovery restarts computation. (4) OCI jobs report progress and checkpoints through the shim; `max_runtime` is set per profile and runtime mode (ADR 0010).
  - **Requirements:** FR-JOB-01, FR-JOB-02. **Dependencies:** WF-01, STO-01, RUN-02.

- **JOB-02 — Recover and cancel analysis jobs through Temporal** · L · M2
  - **Story:** As a workflow author, I want to resume or stop analysis safely, so that long executions remain operable during failures.
  - **Description:** Coordinate attempts, pins and checkpoints through existing Temporal Activities. Risk: R3 (host capabilities).
  - **Acceptance:** (1) Terminate a worker during a checkpoint-aware job and verify resumption from the last committed checkpoint with the same revision. (2) Retry ownership prevents nested retry storms; uncertain external effects are recorded and require application idempotency. (3) Cancellation tests cover compute loops and blocked host I/O, with a bounded process-termination fallback and explicit final status.
  - **Requirements:** FR-JOB-03, FR-JOB-04. **Dependencies:** JOB-01, WF-02.

- **JOB-03 — Qualify mixed interactive and long-running load** · L · M3
  - **Story:** As a SRE, I want to protect interactive work while analysis runs, so that batch jobs cannot monopolize shared capacity.
  - **Description:** Establish separate budgets and measure representative long-job performance and recovery. Risk: R2 (no plan task changes the sandbox, host capabilities, egress, secrets or tenant isolation).
  - **Acceptance:** (1) Run a declared hours-long analysis fixture with measured memory/CPU/checkpoint costs; no native dependency fallback occurs. (2) A mixed-load test enforces configured interactive and batch reservations and reports queueing when quotas are exhausted. (3) Worker scale-down respects jobs/checkpoints, and a failure exercise meets the agreed checkpoint-loss and recovery bounds. (4) The mixed-load test includes OCI pools drawing on the shared tenant slots (ADR 0010).
  - **Requirements:** FR-JOB-05, FR-JOB-06. **Dependencies:** JOB-02, TEN-02, OBS-02.

### OCI. OCI image execution mode

- **Epic EP-OCI: Let authorized projects choose per function to run a catalog-derived OCI image in restricted pods, with Falcone-owned admission and no fallback.**

- **OCI-01 — Authorize OCI mode per tenant and project** · L · M2
  - **Story:** As a platform superadmin, I want to authorize none, all or selected projects of a tenant for OCI mode, so that only approved projects can run container functions.
  - **Description:** Add the superadmin-only OCI authorization (ctr-oci-authorization) that Falcone admission enforces, with revocation drain, BYO flags and an acknowledged residual-risk version. A project is a Falcone workspace. Risk: R3 (tenant isolation and authorization).
  - **Acceptance:** (1) Only the superadmin role can read and write the setting; tenant admins see the effective state read-only, and persona tests deny every other role. (2) Deploy, promotion and every admission path (sync, async, events, Temporal, jobs, retries) reject `oci` for unauthorized projects with `oci_not_authorized`; the Wasm path is unaffected. (3) Revocation drains in an fs-test run: new OCI deploys and promotions are denied at once, new invocations are denied after the superadmin-chosen grace window, in-flight attempts finish within the drain budget or are cancelled, queued work fails non-retryable, triggers pause, and pools are dematerialized while revisions and images are retained; re-authorization restores service. (4) Every change emits an audit event with actor, before/after state, grace window and the acknowledged residual-risk text version. (5) ADR 0010 is recorded.
  - **Requirements:** FR-OCI-01. **Dependencies:** MIG-01, SEC-01, TEN-01, SEC-03.

- **OCI-02 — Declare runtime mode and the OCI function contract** · L · M2
  - **Story:** As a function author, I want to choose `wasm` or `oci` in my manifest, so that each function runs in the mode it needs.
  - **Description:** Add `runtime.mode` to the manifest and revision, and define ctr-oci-function-contract v1 (`POST /invoke`, `GET /healthz`, `$PORT`, the legacy-compatible envelope, signals and filesystem rules). Risk: R3 (defines the tenant-code boundary).
  - **Acceptance:** (1) The manifest and Functions API accept `runtime.mode`, defaulting to `wasm`; a mode or base change creates a new revision, and no automatic mode switch exists anywhere. (2) ctr-oci-function-contract v1 (`/invoke`, `/healthz`, `$PORT`, envelope, signals, filesystem) has a conformance tester. (3) The revision records the mode, `base` as a tagged union: `{kind: catalog, id@version, digest}` or `{kind: byo, image digest, signer identity, attestation ref}`, the function image digest, and the contract and shim versions. (4) Validation before any write rejects `runtime.mode: oci` in a project without OCI authorization, a `kind: catalog` base that is absent, invisible or ineligible under the ADR 0010 D8 lifecycle rule, and a `kind: byo` base in a project without the superadmin BYO flag. (5) The Functions API facade and CLI accept `runtime.mode: oci` and a base in authorized projects, surface `oci_not_authorized` and base errors, and the legacy API fixtures pass in OCI mode.
  - **Requirements:** FR-OCI-02. **Dependencies:** API-02, DEP-02, OCI-01, API-01.

- **OCI-03 — Intake catalog-derived and BYO images** · L · M2
  - **Story:** As a platform operator, I want to have every function image verified and mirrored by digest, so that only known images reach nodes.
  - **Description:** Implement three image paths: P1, assembled by the trusted image assembler onto a catalog base by pure layer assembly; P2, built externally FROM a catalog base digest, with layer-ancestry verification; and P3, BYO images (`base.kind: byo`) only for superadmin-flagged projects. Catalog-membership and ancestry checks apply only to P1 and P2. Every image is copied by digest into the platform intake mirror, and kubelets pull only from there. Risk: R3 (artifact loading and supply chain).
  - **Acceptance:** (1) P1, P2 and P3 follow their verification rules: P1/P2 images must derive from a visible catalog base that is eligible under the ADR 0010 D8 lifecycle rule (`enabled` for a new function or a base switch; `enabled`, or `deprecated` until `window_end`, for a new revision of a function already pinned to that base; `retired` and `disabled` refused for new revisions), and a `kind: byo` image requires the project's superadmin BYO flag, a valid signature against the project's registered keys, a platform-mirror copy and an advisory scan record, with the signer identity and attestation ref recorded; tag-only, wrong-platform, oversized, failed-ancestry and unsigned BYO images are rejected before any pod is rendered. (2) Intake never executes image content, and tenant registry credentials are never logged. (3) Mirror images are retained while referenced and collected only after reference checks; mirror bytes count against the tenant cap.
  - **Requirements:** FR-OCI-03. **Dependencies:** OCI-02, CAT-01, CAT-05, DEP-03.

- **OCI-04 — Serve OCI invocations through a trusted shim** · L · M2
  - **Story:** As an operator, I want to put a trusted component in front of tenant containers, so that admission, identity, limits and outcomes are enforced as in Wasm mode.
  - **Description:** Build the oci-shim sidecar that serves ctr-worker-execution-api for mode oci and forwards each attempt to the function container on a separate localhost port. The dispatcher→shim channel is mutually authenticated TLS. The shim's per-pod identity is a certificate or projected token mounted only into the shim container; the pod sets `automountServiceAccountToken: false`, and the tenant container has no such volume. Because containers in a pod share the network namespace, the dispatcher verifies the shim identity before it sends any credential or request, and the shim→container hop never carries the credential. Risk: R3 (execution boundary).
  - **Acceptance:** (1) The shim implements ctr-worker-execution-api for mode `oci` over mutually authenticated TLS, verifies the dispatcher identity and enforces concurrency and deadlines; the dispatcher verifies the shim's per-pod identity before sending any credential or request, and no identity material is mounted into the tenant container. (2) Cancel goes to the container first, then to the liveness-restart fallback within a bounded time, with honest uncertain outcomes. (3) Progress and checkpoint relay passes the JOB-01 sample job; per-invocation logs reach the Falcone log intake, and pod-level container stdout and stderr are labelled by revision and tenant-scoped. (4) Adversarial test: a tenant container that binds the shim port first, or answers on it, causes the dispatcher to refuse delivery and record an audit or security event, and no credential is leaked.
  - **Requirements:** FR-OCI-04. **Dependencies:** RUN-01, SEC-01, OCI-02, JOB-01.

- **OCI-05 — Reconcile restricted single-revision OCI pools** · L · M2
  - **Story:** As an operator, I want to materialize OCI revisions on demand under restricted PSA, so that inventory does not become pods.
  - **Description:** Extend the namespaced controller to render one Deployment and one headless Service per materialized OCI revision from ctr-oci-pod-template (non-root, read-only root filesystem, all capabilities dropped, seccomp RuntimeDefault, no service-account token). Risk: R3 (sandbox configuration and tenant isolation).
  - **Acceptance:** (1) The controller renders a Deployment and a headless Service per materialized revision only in the tenant's bound OCI namespace; its write set is unchanged. (2) A controller guard refuses any pod spec that deviates from ctr-oci-pod-template. (3) Scale from zero, idle dematerialization and restart convergence pass in fs-test; a 1,000-OCI-function inventory stays within the materialization cap. (4) OCI-05 reuses the restricted posture assertion suite from SEC-02 and runs it against rendered OCI pod templates; SEC-02 consumes no OCI contract and keeps its Wasm M1 scope. The namespace resolver refuses OCI work for a tenant without an `oci_namespace` binding. (5) Admission routes OCI revisions to their materialized pool; preflight checks the OCI prerequisites (OCI namespace, pull Secret, NetworkPolicy enforcement) and blocks OCI mode when enforcement is absent; chart values render the shim, gateway, intake and assembler images and the pull-Secret reference only when the OCI section is enabled.
  - **Requirements:** FR-OCI-05. **Dependencies:** DEP-03, INV-02, SEC-02, TEN-01, OCI-04, OPS-01, INV-01.

- **OCI-06 — Give OCI functions capabilities without credentials** · L · M2
  - **Story:** As a function author, I want to use storage, events, workflows, secrets and HTTP through a local API, so that my container uses Falcone services safely.
  - **Description:** Add ctr-oci-capability-api in the shim, mirroring `falcone:host`, with opaque per-attempt handles, plus the per-namespace egress gateway and OCI-namespace NetworkPolicies. Risk: R3 (host capabilities, egress, secrets).
  - **Acceptance:** (1) Each capability works only during an attempt, with a valid handle, and is scoped from the credential. (2) Allow-listed HTTP succeeds through the egress gateway; direct internet, metadata, API-server and cross-namespace traffic are denied by NetworkPolicy. (3) Secrets arrive only as explicit narrow grants and are absent from environment variables, files, logs and images. (4) In OCI mode the shim holds the execution credential and presents it to the host storage, event and workflow APIs; capability calls are scoped by that credential and audited.
  - **Requirements:** FR-OCI-06. **Dependencies:** SEC-03, STO-01, STO-02, EVT-01, WF-03, OCI-04.

- **OCI-07 — Qualify OCI isolation and record the accepted risk** · L · M2
  - **Story:** As a security engineer, I want to gather adversarial evidence for OCI pods, so that the superadmin knows what they accept.
  - **Description:** Run an adversarial isolation suite against OCI pods and keep the versioned residual-risk register that the authorization flow shows. OCI is enabled for no tenant before this evidence exists and preflight confirms NetworkPolicy enforcement. Risk: R3 (sandbox evidence).
  - **Acceptance:** (1) An fs-test suite covers privilege escalation, setuid, raw sockets, lateral and cross-tenant traffic, metadata, DNS enumeration, fork bomb, disk fill, OOM, CPU noisy neighbour and capability abuse outside attempts. (2) The residual-risk register (same kernel, restricted PSA, side channels, node patch ownership) is versioned and surfaced by OCI-01. (3) Optional `hostUsers: false` and AppArmor availability is recorded as evidence, never assumed.
  - **Requirements:** FR-OCI-07. **Dependencies:** OCI-05, OCI-06, SEC-02.

- **OCI-08 — Meter, limit and observe OCI execution** · L · M3
  - **Story:** As a platform administrator, I want to see per-mode usage and caps, so that OCI capacity is fair and explainable.
  - **Description:** Apply shared tenant slots plus OCI caps, record OCI usage and add the `runtime_mode` metric dimension. Risk: R2 (quota and metering, no boundary change).
  - **Acceptance:** (1) OCI executions consume tenant slots; caps on materialized revisions, warm replicas, pod memory, image bytes and builds are enforced across routers. (2) Usage records for pod-seconds, cold start, idle warm time and image storage reconcile with attempts. (3) Metrics carry `runtime_mode`; a benchmark reports image pull, pod start, init and probe separately against the provisional OCI targets (cold p95 ≤10 s for a node-cached image under 250 MB; warm platform overhead p95 ≤25 ms).
  - **Requirements:** FR-OCI-08. **Dependencies:** TEN-02, OBS-02, OBS-03, OCI-05.

- **OCI-09 — Use OCI as the explicit migration path** · M · M3
  - **Story:** As a platform operator, I want to move legacy functions that fail Wasm qualification to OCI only by explicit choice, so that Knative can retire without hidden fallback.
  - **Description:** Extend migration classification and the backend guard with per-mode verdicts and an explicit target mode. Risk: R2 (migration control in Falcone).
  - **Acceptance:** (1) Classification records the wasm and oci verdicts and the target_mode. (2) The MIG-02 guard allows target `oci` only with an oci-candidate verdict and an authorized project; otherwise the function stays a blocker. (3) MIG-03 retirement counts OCI-migrated workloads and lists remaining blockers.
  - **Requirements:** FR-OCI-09. **Dependencies:** MIG-01, MIG-02, OCI-01, OCI-03.

- **OCI-10 — Develop and deploy OCI functions** · M · M2
  - **Story:** As a developer, I want to use CLI, console and starter support for OCI, so that I can build FROM a catalog base and test locally.
  - **Description:** Add OCI options to the CLI and console, an OCI starter and documentation of the OCI semantics. Risk: R1 (developer experience).
  - **Acceptance:** (1) CLI and console show mode, base, intake status and the warm-reuse notice; OCI options appear only in authorized projects. (2) A starter shows P1 and P2 (`FROM <catalog digest>`) and runs the contract tester locally. (3) Docs state the OCI semantics and that Wasm stays the default.
  - **Requirements:** FR-OCI-10. **Dependencies:** API-01, API-02, API-03, OCI-02.

### CAT. Superadmin base image catalog

- **Epic EP-CAT: Keep one global, audited catalog of Wasm profile components and OCI base images that functions build on and revisions pin.**

- **CAT-01 — Model the global catalog with visibility** · L · M2
  - **Story:** As a platform superadmin, I want to keep one catalog with tenant/project visibility, so that I control which bases each tenant can use.
  - **Description:** Define ctr-base-catalog v1 and its Falcone-owned persistence for both entry kinds (`wasm-profile` and `oci-base`). Risk: R3 (controls code that runs in all tenants).
  - **Acceptance:** (1) Entries hold kind, name@version, digest, source, verification record, status, window, replacement, visibility and defaults; versions are immutable. (2) Only the superadmin mutates entries; reads are filtered by visibility, so no tenant learns of invisible entries. (3) Every mutation emits an audit event.
  - **Requirements:** FR-CAT-01. **Dependencies:** SEC-01, TEN-01.

- **CAT-02 — Select existing bases** · L · M2
  - **Story:** As a platform superadmin, I want to register a base by digest-pinned reference, so that vetted upstream images and components can be used.
  - **Description:** Register Wasm components through artifact validation and OCI bases through image intake into the mirror. Signature verification is optional per entry, and its result is recorded and shown. Risk: R3 (artifact loading).
  - **Acceptance:** (1) A tag resolves to a digest once and is stored as that digest. (2) Wasm components pass artifact validation; OCI bases pass intake; optional signature and provenance results are recorded. (3) A Wasm base is enabled only with WAS-03 qualification evidence for its profile. (4) Registered Wasm components are stored by digest under the `catalog-base` retention class, and each matrix-qualified WAS-03 profile is registered as a catalog Wasm entry.
  - **Requirements:** FR-CAT-02. **Dependencies:** CAT-01, WAS-02, WAS-03, OCI-03, STO-01.

- **CAT-03 — Create bases from recipes** · L · M2
  - **Story:** As a platform superadmin, I want to build a base from a recipe through the trusted build service, so that bases are reproducible.
  - **Description:** Define ctr-base-recipe v1: a Wasm toolchain/profile spec, and a declarative OCI spec without RUN (FROM a registered digest, layers from approved refs, packages from a pinned repository without maintainer scripts, ENV, numeric non-root USER, WORKDIR, ENTRYPOINT/CMD, LABEL). Risk: R3 (trusted build path).
  - **Acceptance:** (1) ctr-base-recipe v1 covers the Wasm toolchain/profile spec and the declarative no-RUN OCI spec; RUN is rejected. (2) Builds run in confined trusted builders with build credentials and produce SBOM and provenance. (3) Results land as draft entries until verification and qualification pass.
  - **Requirements:** FR-CAT-03. **Dependencies:** CAT-01, DEP-01.

- **CAT-04 — Manage base lifecycle and versions** · M · M3
  - **Story:** As a platform superadmin, I want to enable, disable, deprecate and set defaults for bases, so that I can patch bases without breaking functions silently.
  - **Description:** Implement the lifecycle draft → enabled → deprecated (window end, replacement) → retired, plus disabled with optional `block_invocations`, with a provisional 90-day default deprecation window. Risk: R2 (product lifecycle; the block path is reviewed under CAT-05).
  - **Acceptance:** (1) The lifecycle state machine is enforced for `base.kind: catalog` revisions in both runtime modes (Wasm profile and OCI base entries) at deploy, promotion and, for `block_invocations`, invoke; deprecation and retirement do not apply to BYO revisions. Base eligibility follows the lifecycle: a new function, or a function switching base, needs an `enabled` base visible to the project; a new revision of a function already pinned to a base may keep that base while it is `deprecated`, until `window_end`; `retired` and `disabled` bases are refused for new revisions, and `disabled` with `block_invocations` also denies invocations of pinned revisions (`base_disabled`). (2) There is no auto-rebase; a replacement creates candidate revisions that owners must promote. (3) The default alias always targets an `enabled` base and is resolved and pinned at deploy. (4) Lifecycle tests: a grandfathered new revision of a function pinned to a deprecated base is accepted before `window_end` and refused after it; a new function selecting a deprecated base is refused.
  - **Requirements:** FR-CAT-04. **Dependencies:** CAT-01, DEP-02.

- **CAT-05 — Use catalog bases in functions and revisions** · L · M2
  - **Story:** As a developer, I want to pick a base or get my mode's default, so that my builds are reproducible.
  - **Description:** Resolve the selected base, or the per-mode and per-language default alias, at deploy and pin it on the revision; admission enforces visibility, lifecycle eligibility and OCI authorization. Risk: R3 (cross-tenant authorization).
  - **Acceptance:** (1) The manifest selects a catalog base or gets the default; revisions with `base.kind: catalog` pin id@version and digest, and a `kind: byo` revision never requires a catalog base. (2) Admission rejects a catalog base that is absent, invisible or ineligible under the ADR 0010 D8 lifecycle rule (`enabled` for a new function or a base switch; `enabled`, or `deprecated` until `window_end`, for a new revision of a function already pinned to that base; `retired` and `disabled` refused for new revisions), in both runtime modes, (3) The worker refuses a Wasm revision whose profile component digest differs from its pin.
  - **Requirements:** FR-CAT-05. **Dependencies:** CAT-01, API-02, DEP-01, OCI-01.

- **CAT-06 — Superadmin catalog console** · M · M3
  - **Story:** As a platform superadmin, I want to manage the catalog and OCI authorization in console pages, so that I can manage both without calling the API.
  - **Description:** Add superadmin console pages for the catalog and the OCI authorization editor, and a tenant base selector. Risk: R2 (UI over R3 APIs).
  - **Acceptance:** (1) Pages cover list, register, build, verification results, lifecycle, visibility and defaults; destructive actions use the existing confirmation pattern; states are accessible. (2) The tenant view shows a base selector and deprecation notices only for visible bases. (3) UI tests cover persona gating; server-side checks remain authoritative.
  - **Requirements:** FR-CAT-06. **Dependencies:** CAT-01, CAT-04, API-03, OCI-01.


## 5. Remaining questions and ownership

The user has selected Option A. Motivation, Falcone-only interface scope, untrusted code, deployed-function/message scale and quota ownership are settled. They must not be reopened as generic architecture questions. ADR 0010 (27 October 2026) settles the OCI execution mode and the base catalog, including decisions D1–D13.

Implementation discovery still needs:

1. Exact OpenShift patch/version and existing SCC, namespace permissions, allowed image registries and network restrictions.
2. Representative existing JavaScript functions plus the real Python/Go analysis dependencies, input sizes, thread/process needs and expected memory.
3. WebSocket connection count, average/maximum payload, burst duration, fan-out, ordering/durability guarantees and fraction of messages that invoke code.
4. Current SeaweedFS notification configuration and its loss/replay behavior; preserve it until a compatible change is explicitly approved.
5. Long-job duration distribution, checkpoint interval, acceptable lost progress and whether application authors can implement checkpoints.
6. Whether any Model Context Protocol endpoints are actually deployed; WebSockets alone do not establish MCP compatibility.
7. Named maintainers/on-call/security approvers, actual SLA budget and repository/Project destination.
8. For OCI mode: the registry location for the platform intake mirror and the pull Secret in each OCI namespace.
9. Whether the kubelet sets `podPidsLimit`.
10. Whether user namespaces (`hostUsers: false`) and AppArmor are available on the nodes.
11. Who owns node kernel and container-runtime patching.
12. Whether an existing vulnerability scanner can be named; until then, scanning is advisory.

These are qualification inputs, not reasons to stop design work or permission to assume successful tests.

### Responsibility model

| Responsibility | Typical accountable owner | Work |
| --- | --- | --- |
| Product scope | Falcone product owner / technical lead | Supported languages/packages, quotas, compatibility promises and prioritization |
| Runtime | Runtime/platform engineering | Wasmtime integration, host interfaces, execution lifecycle, patches and compatibility |
| Deployment | DevOps/platform engineering | Namespaced chart, image delivery, CI and rollout/recovery procedures |
| Production incidents | SRE/on-call operations, with runtime developers as escalation | Alerts, capacity, failures, restoration and incident review |
| Corporate infrastructure | Existing OpenShift/platform administrators | Approved cluster/SCC/network/storage prerequisites; no new sandbox-runtime installation assumed |
| Security qualification | Application/platform security and independent testers | Threat model, sandbox/host-boundary review, adversarial tests and release evidence |
| Risk acceptance | Deploying organization's designated security/risk owner | Accept residual deployment risks; engineering tests do not substitute for organizational approval |
| OCI authorization and base catalog | Platform superadmin | Authorize OCI per tenant and project, acknowledge the versioned residual-risk text, flag BYO projects and curate catalog bases (ADR 0010) |

Small teams may combine roles, but each responsibility needs a named owner. Open-source maintainers own software/security advisories; self-hosting organizations normally own their deployment and on-call duty unless a support agreement says otherwise.

## 6. Implementation trade-offs and sequence

### Selected and deferred paths

| Choice | Decision | Consequence |
| --- | --- | --- |
| Wasmtime/WASI in restricted pods | Selected baseline | No cluster sandbox runtime required; language/package compatibility and host security become Falcone responsibilities |
| Superadmin-authorized OCI mode (ADR 0010) | Selected as an explicit per-function mode for authorized projects | Catalog-derived images in restricted single-revision pools; same-kernel residual risk accepted per authorization; never a fallback. Unvetted tenant containers and native binaries in Wasm mode remain excluded |
| New operator/CRDs/RuntimeClass | Excluded as mandatory dependencies | PostgreSQL authority and namespaced standard-resource controller |
| Existing Knative | Optional coexistence during migration | No new installation required; no full retirement until all real consumers are resolved |
| External execution pool | Not selected | Not an automatic fallback for unsupported functions |
| Interactive + analysis pools | Selected logical separation | Independent reservations/scaling with quotas governed by Falcone |
| Shared dirty guest instances | Excluded | Fresh execution state in Wasm mode; cache only trusted code artifacts. OCI containers reuse state only within one revision, as documented warm reuse |
| RUN-capable base builds | Deferred | v1 OCI recipes are declarative with no RUN step until a rootless builder is qualified under restricted pods |
| Single-use OCI pods | Deferred | A pod per invocation would need much slower cold paths; OCI uses documented warm reuse instead |
| gRPC/NATS/AMQP additions | Deferred pending measured need | Reuse current WebSockets/Kafka; avoid adding operators/services to solve an unproven gap |
| KEDA | Optional future alternative only | Not required for namespace-only installation; one replica owner remains mandatory |
| Multi-region | Deferred | First prove single-region execution, recovery and corporate-policy compatibility |

### Language enablement

1. Qualify the legacy JavaScript handler/envelope and its real dependencies.
2. Qualify Go with the chosen Wasm/WASI toolchain and required host interfaces.
3. Qualify Python using representative analysis dependencies; maintain an exact package/version catalogue and reject unsupported packages.

This is a testing order, not a claim that one language is universally faster or safer. No profile is enabled for untrusted production use based solely on a hello-world demo.

### Milestones

| Milestone | Deliverable | Exit gate |
| --- | --- | --- |
| M0 — feasibility | Accepted ADR, contract inventory, minimum restricted-OpenShift Wasmtime fixture | MIG-01 and WAS-01; exact policy/version evidence, no privilege escalation |
| M1 — secure execution | Host capability contract, qualified profiles, fresh-instance runner, Falcone auth/quotas, durable invocations and namespaced packaging | WAS-02/03 plus native slice; “native” means Falcone-owned backend, not tenant native machine code |
| M2 — platform behavior | Wasm artifact pipeline, routes/rollbacks, pool activation, events/DLQ, Temporal, checkpoint-aware jobs and HA | Recovery, isolation, package compatibility and mixed-load evidence |
| M3 — migration | WebSocket load qualification, hours-long analysis tests, legacy migration, restore/upgrade and retirement | All mandatory compatible-workload contracts pass; unresolved workloads block retirement |
| M2 (OCI) — authorized OCI mode and base catalog | After the Wasm M1 exit: OCI-01 authorization, CAT-01/02/03/05 catalog core, OCI-02..07 contract, intake, shim, pools, capabilities and isolation evidence, OCI-10 developer support | OCI-07 isolation evidence and preflight-confirmed NetworkPolicy enforcement before OCI is enabled for any tenant |
| M3 (OCI) — OCI operations and migration path | OCI-08 metering and benchmarks, OCI-09 explicit migration path, CAT-04 lifecycle and CAT-06 console | OCI-migrated workloads counted in retirement; no automatic mode or base change anywhere |

### First executable qualification specification

- Deploy only approved standard namespaced resources with the available identity; record server-side admission and actual startup.
- Run a valid module and a deliberately rejected native/invalid artifact.
- Execute an infinite-loop guest, guest-memory exhaustion case and blocked host call; prove bounded independent cancellation.
- Attempt cross-tenant object/handle access and forbidden filesystem/network operations; prove denial and scoped audit.
- Run a real legacy JavaScript function and real analysis dependencies; publish supported and unsupported behaviors.
- Kill a checkpoint-aware analysis execution and resume from a committed SeaweedFS checkpoint through Temporal.
- OCI fixture (ADR 0010): start a restricted single-revision OCI pod, run an escape and lateral-movement suite against it, and prove revocation drain.

Deliverables from this spike are a reproducible fixture, pinned engine/toolchain manifests, compatibility results, resource measurements and a go/no-go report. This package specifies that work; it does not claim the fixture or tests have run.

### Backlog and import notes

The revised package contains **16 epics, 58 stories and 100 requirements**. Existing IDs are retained; WAS (qualification/capabilities) and JOB (long-running recovery) are added, and ADR 0010 adds OCI (OCI execution mode) and CAT (superadmin base catalog). Every requirement maps to a story and the dependency graph is checked for cycles.

The importer remains preview-first and preserves existing issue bodies. If an older package was imported elsewhere, importing this revision does not automatically rewrite those existing issues; review and update their changed scope separately. No GitHub issues or deployments have been changed by producing this revision. The ADR 0010 revision changes 31 published stories and the EP-RUN, EP-SEC and EP-MIG goals; those issue bodies need a separately reviewed update, and the 18 OCI and CAT issues are not yet published.

### Definition of done

- Linked requirements and explicit positive/negative acceptance criteria pass on supported profiles.
- Integration evidence uses actual relevant services and actual corporate admission policies.
- No native or OCI fallback, implicit mode or base change, implicit privilege escalation or cross-tenant capability leakage is accepted.
- Unsupported dependencies, timeout uncertainty and checkpoint limitations are visible to users.
- Engine, toolchain, package, chart and data-format versions are pinned and recoverable.
- Production qualification, SLO claims and Knative retirement require evidence, not design approval alone.


## Sources and evidence

Primary sources retrieved on 21 October 2026. Live documentation can change; pin versions before implementation. Repository links below pin the Falcone evidence snapshot. Recommendations, effort sizes and SLO targets are architectural judgments.

- **S1:** [Falcone README](https://github.com/gntik-ai/falcone/blob/bde62dd1e2e91f957b1dc6b8212dfbbaee977a7f/README.md).
- **S2:** [Falcone Function executor](https://github.com/gntik-ai/falcone/blob/bde62dd1e2e91f957b1dc6b8212dfbbaee977a7f/apps/control-plane/function-executor.mjs).
- **S3:** [Falcone Temporal functions.invoke Activity](https://github.com/gntik-ai/falcone/blob/bde62dd1e2e91f957b1dc6b8212dfbbaee977a7f/apps/workflow-worker/src/activities/functions-invoke.mjs).
- **S4:** [Managed-Knative proposed integration](https://github.com/gntik-ai/falcone/blob/bde62dd1e2e91f957b1dc6b8212dfbbaee977a7f/docs/installation/managed-knative-proposed.md).
- **S5:** [Existing managed-Knative design](https://github.com/gntik-ai/falcone/blob/bde62dd1e2e91f957b1dc6b8212dfbbaee977a7f/openspec/changes/add-managed-knative-serving/design.md).
- **S6:** [Knative Serving architecture](https://knative.dev/docs/serving/architecture/).
- **S7:** [Kourier](https://github.com/knative-extensions/net-kourier).
- **S8:** [Knative event delivery failures](https://knative.dev/docs/eventing/event-delivery/).
- **S9:** [Falcone runtime-status and cleanup contract](https://github.com/gntik-ai/falcone/blob/bde62dd1e2e91f957b1dc6b8212dfbbaee977a7f/docs/reference/architecture/knative-runtime-status.md).
- **S10:** [Fission architecture](https://fission.io/docs/architecture/).
- **S11:** [Fission executor](https://fission.io/docs/architecture/executor/).
- **S12:** [OpenWhisk architecture](https://github.com/apache/openwhisk/blob/master/docs/about.md).
- **S13:** [OpenWhisk reference and namespaces](https://github.com/apache/openwhisk/blob/master/docs/reference.md).
- **S14:** [OpenWhisk documentation](https://openwhisk.apache.org/documentation.html).
- **S15:** [Nuclio architecture](https://docs.nuclio.io/en/stable/concepts/architecture.html).
- **S16:** [Nuclio repository](https://github.com/nuclio/nuclio).
- **S17:** [Fn repository](https://github.com/fnproject/fn).
- **S18:** [Fn agent API](https://pkg.go.dev/github.com/fnproject/fn@v0.3.770/api/agent).
- **S19:** [OpenFaaS edition/pricing terms](https://www.openfaas.com/pricing/).
- **S20:** [OpenFaaS scale-to-zero](https://docs.openfaas.com/openfaas-pro/scale-to-zero/).
- **S21:** [Kubeless maintenance notice](https://github.com/vmware-archive/kubeless-website).
- **S22:** [Temporal Activity execution](https://docs.temporal.io/activity-execution) and [Activity failure detection](https://docs.temporal.io/encyclopedia/detecting-activity-failures).
- **S23:** [KEDA concepts](https://keda.sh/docs/2.22/concepts/) and [HTTP add-on scaling](https://keda.sh/http-add-on/0.16/concepts/scaling/).
- **S24:** [SeaweedFS S3 compatibility](https://github.com/seaweedfs/seaweedfs/wiki/Amazon-S3-API) and [filer notifications](https://github.com/seaweedfs/seaweedfs/wiki/Filer-Notification-Webhook). Deployed-version verification is mandatory; S3 compatibility does not prove AWS event-notification parity.
- **S25:** [GitHub sub-issues REST API](https://docs.github.com/en/rest/issues/sub-issues) and [GitHub CLI project item-add](https://cli.github.com/manual/gh_project_item-add).

- **S26:** [Wasmtime security](https://docs.wasmtime.dev/security.html).
- **S27:** [WASI capability security](https://wasi.dev/security).
- **S28:** [Wasmtime configuration and host-call interruption limits](https://docs.wasmtime.dev/api/wasmtime/struct.Config.html).
- **S29:** [Python WebAssembly availability](https://docs.python.org/3/library/intro.html#webassembly-platforms).
- **S30:** [Kubernetes release lines](https://kubernetes.io/releases/), [OpenShift 4.20 release notes](https://docs.redhat.com/en/documentation/openshift_container_platform/4.20/html/release_notes/ocp-4-20-release-notes), and [SeaweedFS releases](https://github.com/seaweedfs/seaweedfs/releases). Version observations were checked earlier in this conversation on 21 October 2026; pin and requalify at implementation.
