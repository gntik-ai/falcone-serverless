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
  - **Acceptance:** (1) Installation and workload startup pass with the actual namespace identity and existing SCC, without cluster writes, host privileges or custom RuntimeClass. (2) Guests cannot invoke native processes, load native libraries or obtain Kubernetes/admin credentials; dangerous imports are rejected. (3) Sandbox failure or unsupported features stop execution; there is no native fallback. Worker and compilation isolation tests are documented.
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
  - **Acceptance:** (1) SQL/RLS, API, log, artifact and activation tests reject adjacent tenants and unauthorized sibling workspaces. (2) Same function name in two tenants produces distinct owned runtime resources. (3) Unknown namespace mappings fail before Kubernetes writes; tenant input cannot choose a privileged namespace. (4) The namespace-bindings contract v1 includes an optional preprovisioned `oci_namespace` per tenant (ADR 0010).
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
  - **Acceptance:** (1) Preflight using the deployment identity proves all required namespaced verbs; a rendered manifest audit contains no cluster-scoped resources, privileged pods or custom SCC/RuntimeClass. (2) Actual pod startup and invocation pass on qualified Kubernetes lines and the supplied OpenShift patch release under its existing SCC. (3) Preprovisioned namespace, gateway, storage, broker and RBAC prerequisites are explicit; Falcone never silently creates/adopts cluster infrastructure. (4) The serverless chart values contract v1 includes an OCI section that is disabled by default and renders no OCI resources (ADR 0010).
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
  - **Acceptance:** (1) Each enabled language has exact runtime/compiler/package versions, allowed interfaces, tested examples and unsupported-feature diagnostics. (2) The selected legacy JavaScript function and representative Python analysis pass expected results and dependency behavior; a failure blocks that profile or migration claim. (3) Host-interface fuzz/adversarial tests, fresh-instance leakage tests and a worker-crash recovery exercise are recorded before untrusted production enablement. (4) Matrix entries reserve a catalog entry id; compatibility diagnostics may name OCI as an explicit alternative but never select it (ADR 0010).
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
  - **Acceptance:** (1) The controller renders a Deployment and a headless Service per materialized revision only in the tenant's bound OCI namespace; its write set is unchanged. (2) A controller guard refuses any pod spec that deviates from ctr-oci-pod-template. (3) Scale from zero, idle dematerialization and restart convergence pass in fs-test; a 1,000-OCI-function inventory stays within the materialization cap. (4) Restricted posture assertions (SEC-02) run against rendered OCI pod templates, and the namespace resolver refuses OCI work for a tenant without an `oci_namespace` binding. (5) Admission routes OCI revisions to their materialized pool; preflight checks the OCI prerequisites (OCI namespace, pull Secret, NetworkPolicy enforcement) and blocks OCI mode when enforcement is absent; chart values render the shim, gateway, intake and assembler images and the pull-Secret reference only when the OCI section is enabled.
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

