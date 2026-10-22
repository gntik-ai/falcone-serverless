# Option A architecture review

Review date: 2026-10-22. Evidence baseline: source SHA `9819ee51` (`9819ee51754bcbffd345fbec23db36b4d1807d1a`). In this review and the ADRs, `design.md` means `docs/falcone-serverless-design.md`. Source line citations refer to that immutable baseline, not to a future regenerated report.

## Scope and decision boundaries

This is an additive documentation review of the closed 15-item inventory below: design §5 items 1–7 and eight ADR topics. It does not claim to cover other open points in §3 or §6. [ADR 0001](adr/0001-execution-model-option-a.md) records the operator's accepted execution direction of 2026-10-21; [ADR 0002](adr/0002-repository-ownership.md) records the operator's accepted repository split of 2026-10-22. The decider is the operator role, not an invented personal identity.

ADRs 0003–0009 are Proposed. Their Decision sections are recommendations, not enacted policy or implementation authorization. Acceptance of ADRs 0004–0006 requires R3 review: a second reviewer and explicit operator approval (`AGENTS.md:24`). Other changes touching sandbox, host capabilities, egress, secrets or tenant isolation retain that same gate. No existing hard rule is relaxed.

`worker/` and `controller/` are placeholders (`worker/src/main.rs:3-4`, `controller/main.go:3-5`), not runtime implementations; they are not reviewed further here. There is no Wasmtime dependency in `worker/Cargo.toml`. Cranelift/Winch/Pulley suitability, executable-memory behavior and the future exact version pin are unverified. No runtime, cluster, security or performance qualification has been performed in this review.

ADR 0001 addresses only MIG-01 (#48) criterion (2), `docs/backlog.md:257`. Criteria (1), contract inventory, and (3), source dependency classification, remain outside this change and continue to gate MIG-01 and dependent WAS-01 (#51), `docs/backlog.md:280`. This review neither runs WAS-01 nor declares either story complete.

## Findings

Severity describes the consequence of leaving the question unresolved before implementation; it does not change this additive documentation ChangeSet's R1 risk. ADR references marked Proposed describe a candidate resolution, not closure. Every row has a disposition and an accountable review role.

| Finding | Severity | Design section and baseline evidence | Proposed change / disposition | Stories | ADR / accountable role |
| --- | --- | --- | --- | --- | --- |
| F1: Direction lacks a durable decision record | Gate | Accepted direction; §6, design.md:3,714-724 | Record the existing Option A decision and scale facts; retain feasibility and remaining MIG-01 gates | MIG-01 #48; WAS-01 #51 | [0001](adr/0001-execution-model-option-a.md), Accepted; operator |
| F2: Repository destination guidance is conditional | Ownership | §3, design.md:252-263; docs/README.md:46 | Record AGENTS.md:8-16 as the implementation split; leave operational assignments explicit | MIG-01 #48; DEP-03 #23; OPS-01 #45 | [0002](adr/0002-repository-ownership.md), Accepted; platform operator |
| F3: Compilation backend, exact pin and patch ownership lack qualification | Qualification | §3, design.md:256,265,285 | Compare candidates under actual policy; choose an exact pin only after evidence; assign CVE triage and cache invalidation | RUN-01 #18; WAS-01 #51; WAS-02 #52 | [0003](adr/0003-wasmtime-compilation-strategy.md), Proposed; runtime maintainer |
| F4: Process strategy and latency goals need a joint decision | Security gate | §3, design.md:287,303,353-354 | Qualify fresh trusted processes and fresh guests first; require evidence before considering shared-process instances | RUN-03 #20; SEC-02 #34; WAS-03 #53 | [0004](adr/0004-per-invocation-isolation.md), Proposed, R3; security approver |
| F5: Host egress and infrastructure enforcement ownership are implicit | Security gate | §3, design.md:271-273,296 | Recommend host allow-list plus operator-owned network enforcement; controller never writes NetworkPolicies | SEC-03 #35; WAS-02 #52; OPS-01 #45 | [0005](adr/0005-egress-enforcement.md), Proposed, R3; platform operator and security approver |
| F6: OpenBao authentication and execution-credential issuer are unspecified | Security gate | §2/§3, design.md:172,271,301 | Recommend existing External Secrets delivery, host-only scope and Falcone-issued execution credentials; require integration evidence | SEC-03 #35; TEN-01 #36; STO-02 #40 | [0006](adr/0006-pool-identity-and-openbao.md), Proposed, R3; Falcone maintainer and security approver |
| F7: Pool granularity and 400 versus 1,000 slots could be mistaken for capacity guarantees | Capacity | Confirmed requirements; §3, design.md:23,31,287,324 | Recommend tenant/profile pools and separate class reservations; distinguish quota ceiling from worked demand example | INV-02 #25; TEN-02 #37; JOB-03 #56 | [0007](adr/0007-pool-topology-and-sizing.md), Proposed; platform operator |
| F8: Instrumentation has no explicit backend placement responsibility | Operations | §3, design.md:246 | Recommend existing operator-managed backends and approved namespaced export; require scope, retention and outage evidence | OBS-01 #42; OBS-02 #43; OBS-03 #44 | [0008](adr/0008-observability-placement.md), Proposed; platform operator |
| F9: PostgreSQL reuse leaves schema/migration authority implicit | Data integrity | §2/§3, design.md:173-174,260,275 | Recommend Falcone-owned authoritative schema/migrations and versioned runtime projections; require upgrade/restore evidence | STO-03 #41; DEP-03 #23; OPS-03 #47 | [0009](adr/0009-schema-migration-ownership.md), Proposed; Falcone maintainer |

## Coverage inventory

Exactly 15 rows follow. The first seven preserve the §5 discovery inventory; the other eight cover ADRs 0002–0009. ADR 0001 is the accepted baseline, not an extra unresolved topic. A Proposed ADR addresses its row in its Decision section but does not close qualification or approval work. Owner roles are suggested accountable roles for operator confirmation in review; they are not claims that personnel or an SLA have already been assigned.

| Item | Source at baseline | Resolving ADR or accountable owner | Remaining evidence / disposition |
| --- | --- | --- | --- |
| §5 item 1: OpenShift version, SCC, namespace, registry and network prerequisites | design.md:688 | Platform operator | Supply exact policy/version and permission evidence to WAS-01 #51; no cluster access is assumed |
| §5 item 2: Real JS/Python/Go functions, dependencies and resource needs | design.md:689 | Falcone maintainer | Supply representative workloads and classify dependencies for MIG-01 #48 and WAS-03 #53 |
| §5 item 3: WebSocket connections, payload, bursts, fan-out, guarantees and trigger fraction | design.md:690 | Falcone maintainer | Agree a measured traffic profile for INV-03 #26; 100,000 messages/s is not an invocation-rate claim |
| §5 item 4: SeaweedFS notification loss/replay behavior | design.md:691 | Storage operator | Supply current configuration and recovery evidence for STO-02 #40; preserve behavior pending approval |
| §5 item 5: Job duration, checkpoints and acceptable lost progress | design.md:692 | Falcone product owner | Agree application checkpoint feasibility and loss budgets with authors for JOB-01 #54 and JOB-03 #56 |
| §5 item 6: Actually deployed MCP endpoints | design.md:693 | Falcone maintainer | Inventory actual contracts for MIG-03 #50; WebSockets alone do not prove MCP compatibility |
| §5 item 7: Maintainers, on-call, security approvers, SLA and repository/Project destination | design.md:694 | [0002](adr/0002-repository-ownership.md) for repositories; platform operator for remaining assignments | Repository split is Accepted; confirm operational roles, SLA budget and Project destination for MIG-01 #48 and OPS-01 #45 |
| Repository ownership | §3, design.md:252-263; docs/README.md:46 | [0002](adr/0002-repository-ownership.md) | Accepted split; coordinate future repository-specific work without transferring issues here |
| Compilation, version pin and CVE policy | §3, design.md:256,265,285 | [0003](adr/0003-wasmtime-compilation-strategy.md) | Proposed; runtime maintainer supplies WAS-01 #51 comparison and exact-pin/patch evidence |
| Per-invocation isolation | §3, design.md:287,353-354 | [0004](adr/0004-per-invocation-isolation.md) | Proposed; runtime maintainer supplies lifecycle/latency evidence; second reviewer and operator approval required |
| Egress enforcement | §3, design.md:271-273,296 | [0005](adr/0005-egress-enforcement.md) | Proposed; platform operator supplies enforcement evidence; second reviewer and operator approval required |
| Pool identity and OpenBao | §2/§3, design.md:172,271,301 | [0006](adr/0006-pool-identity-and-openbao.md) | Proposed; Falcone maintainer and platform operator supply issuer/delivery evidence; second reviewer and operator approval required |
| Pool topology and sizing | §3, design.md:287,324; confirmed quotas, design.md:23,31 | [0007](adr/0007-pool-topology-and-sizing.md) | Proposed; platform operator and Falcone product owner reconcile demand, quotas and class budgets from measurements |
| Observability placement | §3, design.md:246 | [0008](adr/0008-observability-placement.md) | Proposed; platform operator supplies backend, retention and scoped retrieval evidence |
| Schema/migration ownership | §3, design.md:260,275 | [0009](adr/0009-schema-migration-ownership.md) | Proposed; Falcone maintainer supplies versioned data contracts and migration/restore plan |

The seven discovery rows remain qualification work, except for repository ownership within item 7. Roles do not substitute for collected evidence. No additional §3/§6 inventory is implied.

## Proposed design edits — unapplied diff

The diff below is **unapplied**. It targets `docs/report-front.md`, `docs/report-architecture.md` and `docs/report-tail.md`, the generator inputs, not the generated design. `docs/build_package.py:79-86` rebuilds `docs/falcone-serverless-design.md` from those fragments and generated sections. A later separately reviewed change may apply approved edits and regenerate the package; this ChangeSet does neither. Line references in this review will need revalidation after regeneration; section references remain the conceptual anchors. Proposed language below must stay explicitly provisional until the associated ADR is accepted.

```diff
--- a/docs/report-front.md
+++ b/docs/report-front.md
@@ -20,7 +20,7 @@
 | Tenant trust | Arbitrary untrusted code within qualified Wasm profiles; no arbitrary native execution |
 | Scale | Approximately 20 tenants and 10,000 deployed function definitions |
 | Messaging | Approximately 100,000 aggregate WebSocket/event messages per second; mostly messaging, not 100,000 function invocations/second |
-| Execution quota | Initial configurable estimate of 20 simultaneous executions per tenant; roughly 400 total if all 20 tenants use their quota |
+| Execution quota | Initial configurable estimate of 20 simultaneous executions per tenant; roughly 400 aggregate quota slots, not measured compute capacity; see proposed ADR 0007 |
 | Analysis | Extended asynchronous jobs, configurable duration limits, progress, cancellation and application checkpoint/resume |
 | Kubernetes | Latest two qualified stable minor lines, currently 1.37 and 1.36 in the research snapshot; patch-pinned releases |
 | OpenShift | 4.20 / Kubernetes 1.33 family; exact OpenShift patch bundling the user's Kubernetes 1.33.12 must be confirmed from cluster metadata |
--- a/docs/report-architecture.md
+++ b/docs/report-architecture.md
@@ -24,6 +24,8 @@

 OpenTelemetry instruments gateways, dispatch, guest lifecycle and host calls. Telemetry and results always carry verified tenant/workspace scope. The diagram does not imply guest access to raw infrastructure credentials.

+Proposed in ADR 0008: use existing operator-managed observability backends through an approved namespaced export path. Backend placement, retention, scoped retrieval and outage behavior need operator evidence before acceptance.
+
 ### Technology and deployment model

 | Component | Selected approach | Reason |
@@ -43,6 +45,8 @@

 Wasmtime/WASI capability isolation is the selected boundary for untrusted guest code. It requires a correctly secured host interface and patch policy; it is not a claim of zero residual risk. [S26–S27]

+Proposed in ADR 0003: compare Cranelift, Winch and a possible Pulley path during WAS-01. Backend support, executable-memory behavior and the exact Wasmtime pin are unverified. The runtime maintainer would own advisory triage, reviewed patch pins, cache invalidation and requalification; no candidate is a promised fallback.
+
 ### Deployment authority and permission boundary

 Use preprovisioned namespaces. Falcone must not require namespace creation, cluster-wide role grants or admission configuration changes. The installer needs explicitly granted namespaced permissions, not merely the ability to create a Deployment.
@@ -51,7 +55,11 @@

 Corporate-owned ingress/TLS, Kafka, storage, database, network policy enforcement and existing SCC remain prerequisites. An application-only design does not create those facilities or grant itself permissions.

+Proposed in ADR 0005, pending R3 review: the platform operator would own NetworkPolicy enforcement, while the host enforces invocation-scoped HTTP grants, redirects and DNS checks. The pool controller would still write only Deployments, Services and Leases. Missing enforcement blocks qualification.
+
 PostgreSQL remains authoritative for Function/Revision/Trigger/Job records. API transactions and outbox events drive standard Kubernetes worker-pool resources and route/config generations. No CRDs are required. Users submit declarative manifests through Falcone's API; they do not edit worker resource projections directly.
+
+Proposed in ADR 0009: Falcone would own authoritative PostgreSQL schemas and migrations, with versioned worker/controller projections, scoped constraints, attempt fencing and upgrade/restore evidence. No separately authoritative runtime database is proposed.

 The native pool scaler is the only writer of replica counts. Desired-state reconciliation must not reset those counts. KEDA/HPA is not a mandatory dependency; any future alternate scaler requires an exclusive ownership handoff.

@@ -64,6 +72,8 @@
 Validate uploaded modules before bounded compilation. Accept no tenant-produced native precompiled cache: native engine caches are generated only by trusted platform components and keyed by artifact digest, engine, architecture and configuration. Cache provenance is distinct from the portability of raw Wasm.

 Begin with tenant/profile-specific worker pools. Each invocation receives a fresh guest instance and scoped handles. Pool capacity and trusted compiled code can be reused, but dirty guest memory, credentials, temporary files and handles cannot. Use a fresh trusted execution process per invocation for the first qualified implementation where feasible; benchmark the startup cost before enabling more aggressive in-process multiplexing. Process separation supplements the Wasm boundary; it does not replace sandbox validation.
+
+Proposed in ADR 0004, pending R3 review: qualify fresh trusted processes first against both latency targets, leakage and cancellation tests. In-process fresh instances would need separate evidence and approval; a missed latency target does not authorize weaker isolation.

 Guest code cannot launch native processes or arbitrary host plugins. The trusted worker may launch the fixed execution harness; that is platform behavior, not a guest capability.

@@ -78,6 +88,8 @@
 - Structured logging, cancellation state and explicit progress/checkpoint reporting.

 Authorization is derived from verified invocation context, never caller-chosen guest fields. Credentials remain in the host binding layer unless a specific reviewed API explicitly grants a narrow secret to guest code. Guest handles are execution-scoped and invalid after completion.
+
+Proposed in ADR 0006, pending R3 review: retain OpenBao delivery through existing External Secrets, with operator-owned authentication and host-only scoped material; pools would not log in directly. Falcone admission/dispatch would issue short-lived, pool/attempt-bound execution credentials. Authentication, rotation, revocation and issuer contracts remain unverified review deliverables.

 Wasmtime fuel/epoch interruption applies to guest computation, not every blocking host call. Host network/storage operations require their own timeout, cancellation and memory limits; a bounded process termination fallback handles unresponsive executions. [S28]

@@ -101,6 +113,8 @@
 Scale connection gateways by connection/message load, dispatchers by backlog, and execution pools by admitted running/waiting work and profile capacity. A basic pool demand estimate is running plus admitted waiting executions divided by per-replica execution capacity, bounded by Falcone quotas and stabilization policy. Validate the control loop under burst/idle/failure tests.

 Track incoming messages, outgoing deliveries after fan-out, bytes, triggered invocations and open connections separately. For example, 1% of 100,000 messages triggering one function means 1,000 invocations/second. At a one-second average duration, that needs roughly 1,000 active execution slots; the provisional 400-slot quota would queue or reject excess work. Long jobs further consume capacity, so reserve interactive and analysis budgets.
+
+Proposed in ADR 0007: retain tenant/profile pools with interactive/analysis reservations within Falcone quotas. The 1,000-slot example expresses demand, while 400 is an aggregate quota assumption; neither measures provisioned capacity. Replica scaling cannot override quotas, and excess work needs bounded queueing or rejection.

 ### Long-running analysis

--- a/docs/report-tail.md
+++ b/docs/report-tail.md
@@ -11,6 +11,8 @@
 5. Long-job duration distribution, checkpoint interval, acceptable lost progress and whether application authors can implement checkpoints.
 6. Whether any Model Context Protocol endpoints are actually deployed; WebSockets alone do not establish MCP compatibility.
 7. Named maintainers/on-call/security approvers, actual SLA budget and repository/Project destination.
+
+ADR 0002 records the accepted 2026-10-22 repository split in AGENTS.md:8-16: worker/controller here, API/quotas/admission in gntik-ai/falcone, and chart in gntik-ai/falcone-charts. The platform operator still owns maintainers, on-call/security assignments, SLA budget and Project destination. Discovery items 1-6 remain open with accountable roles in architecture-review.md; repository placement does not complete them.

 These are qualification inputs, not reasons to stop design work or permission to assume successful tests.

```

## Validation and handoff

The docs CI job only validates JSON (`.github/workflows/ci.yml:55-66`). Its two commands are `python3 -m json.tool docs/backlog.json` and `python3 -m json.tool docs/requirements-and-stories.json`, with output discarded. JSON validity does not validate ADR decisions, references or inventory coverage.

Review checks additionally cover the exact ten added paths, unchanged existing sources, nine ADR templates and statuses, source-backed line/section/story references, R3 gates, 15 coverage rows and nonempty findings/owner cells. Each ADR-linked coverage row maps to that ADR's Decision section: 0002 covers both repository rows and explicitly limits item 7; 0003 covers compilation/pinning/patching; 0004 isolation; 0005 host/network enforcement; 0006 delivery/issuer identity; 0007 topology and demand; 0008 placement; 0009 data authority. The embedded diff can be checked for applicability without applying it.

The platform operator should confirm owner-role assignments in review. Later acceptance of proposals, applying report edits, any cross-repository work and runtime qualification require separate changes. No issues are created or transferred here. Rollback consists of reverting these ten added documents and has no runtime effect.
