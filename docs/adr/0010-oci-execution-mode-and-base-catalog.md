# ADR 0010: Superadmin-authorized OCI execution mode and base catalog

## Status

Accepted — operator, 2026-10-27. This ADR records the operator decisions S1–S4 of 2026-10-27. It also records the operator's instruction to adopt every recommendation D1–D13 of the OCI-mode proposal; each is listed below as decided. The decision is R3 (`AGENTS.md`, hard rules): the operator approved it, and each change that implements it still needs a second reviewer. Acceptance records direction, not qualification. OCI isolation, latency and capacity remain unverified until OCI-07 and OCI-08 provide evidence.

## Context

ADR 0001 records Option A: Wasmtime/WASI inside restricted pods, with no arbitrary tenant containers. The design excluded tenant Linux containers in its scope (`docs/falcone-serverless-design.md:9`), in FR-RUN-01, FR-SEC-04 and FR-DEP-01, and in its §6 trade-off table. The `AGENTS.md` hard rules for tenant artifacts and capability access did the same, and they are relaxed only by an accepted ADR. This is that ADR.

The operator asked for a classic OCI image mode next to Wasm and settled four points on 2026-10-27:

- **S1, OCI authorization.** The platform superadmin sets OCI mode per tenant to *none*, *all projects* or *selected projects*. The runtime mode is chosen per function and is accepted only when the function's project is authorized. Wasm is the default and is always available. Falcone enforces and audits the setting. Only the superadmin can change it; tenant admins cannot.
- **S2, accepted risk.** OCI functions share the node kernel and run under restricted Pod Security only. The superadmin opts into this residual risk when authorizing a project. It is recorded, not treated as a blocker.
- **S3, base catalog.** One global catalog, managed by the superadmin, holds Wasm runtime/profile components and OCI base images. Each base can be limited to particular tenants or projects. The superadmin selects existing bases by registry digest or creates them from a recipe.
- **S4, image origin.** An OCI function image must be built FROM a catalog base. A project may bring its own fully built image ("BYO") only when the superadmin flags that project.

A project is a Falcone workspace: the serverless design scopes resources by `tenant_id` and `workspace_id`, and Falcone's quickstart calls a workspace the project boundary. The MIG-01 (#48) contract inventory confirms this mapping in its authorization row.

The new work is EP-OCI (OCI-01..10) and EP-CAT (CAT-01..06), with requirements FR-OCI-01..10 and FR-CAT-01..06 (`docs/falcone-serverless-design.md:225-247`). Thirty-one published stories change; `docs/backlog.json` holds their updated scope.

## Decision

**Runtime mode.** Each function declares `runtime.mode`. `wasm` is the default and needs no authorization. `oci` is accepted only for projects that the platform superadmin has authorized in the tenant (none, all or selected). Falcone enforces this at deploy, at promotion and at every admission path: sync, async, events, Temporal, jobs and retries. Every change is audited and superadmin-only. The mode is recorded on the immutable revision. Nothing switches a function between modes automatically, and OCI is never a fallback: a failed Wasm qualification may *name* OCI in its diagnostics, but adopting OCI is a new revision that an authorized user deploys in an authorized project.

**OCI execution.** OCI functions run as images built FROM a global, superadmin-managed catalog base (BYO only in flagged projects). Each image is copied by digest into the platform intake mirror. The functions run in single-revision, restricted, non-root, read-only-rootfs pods in a dedicated per-tenant namespace. A trusted shim stands in front of each container and serves the worker execution API and a localhost capability API. The execution credential never enters the tenant container. The controller's write set is unchanged: OCI pools are standard Deployments and headless Services in preprovisioned namespaces, with no operator, CRD, ClusterRole, RuntimeClass, SCC, host access, or Pod, Job or Secret writes. NetworkPolicy enforcement is mandatory for OCI mode; if preflight cannot confirm it, OCI mode is blocked.

**Base catalog.** Wasm bases (`kind: wasm-profile`) and OCI bases (`kind: oci-base`) share one catalog. Each `name@version` is immutable and bound to one digest. A per-mode, per-language default alias is resolved at deploy and pinned on the revision. Visibility is global by default, with an optional allowlist of tenants and tenant/workspace pairs; using an OCI base also requires OCI authorization of the project. Enabling a Wasm base requires WAS-03 qualification evidence for its profile. Wasm catalog components follow ADR 0003's pin and CVE policy.

**Image paths.** P1: the trusted image assembler layers source or a prebuilt artifact onto a catalog base by pure layer assembly, with no RUN step and no tenant hook. P2: the tenant's CI builds `FROM <catalog base digest>`, and intake verifies that the image's bottom layers equal the base manifest's layers. P3 (BYO): any base, only in a flagged project, with a mandatory signature. Ancestry is provenance and patch reach, not a security boundary; the restricted pod is the boundary.

**Decisions D1–D13 (all decided 2026-10-27).**

| ID | Topic | Decision |
| --- | --- | --- |
| D1 | Publication hold and amendment route | Append the ADR 0010 amendments to the 9 published contract tasks before delivery, and hold the 13 next-layer candidate tasks until they are changed. Publishing OCI and the catalog later as v2 contracts is rejected. |
| D2 | Revocation of OCI authorization | Drain. New OCI deploys and promotions are denied at once, in the same transaction. New invocations are denied with `oci_not_authorized` (non-retryable) after a grace window the superadmin chooses at revocation (default 0, provisional maximum 24 h). In-flight attempts finish within the drain budget or are cancelled. Queued work fails non-retryable; triggers pause and keep their deliveries, as in TEN-03. Pools are dematerialized; revisions and images are retained, so re-authorization restores service. Owners may explicitly roll back to a retained Wasm revision. |
| D3 | Tenant boundary for OCI pods | One dedicated, preprovisioned OCI namespace per tenant. Dedicated nodes are an optional tier where the operator can label or taint nodes. |
| D4 | Invocation contract | The trusted shim plus Falcone container contract v1 (`ctr-oci-function-contract`: `POST /invoke` with `{params, context}`, a legacy-compatible `{status, result, logs}` envelope, `GET /healthz`, `$PORT`). Knative-style passthrough is added as v1.1 only if the MIG-01 inventory finds Knative-style functions. |
| D5 | Capability access | A localhost capability proxy in the shim (`ctr-oci-capability-api`, mirroring `falcone:host`). The container gets only an opaque per-attempt handle; the credential stays outside tenant memory. |
| D6 | Image supply | An intake mirror in a platform-controlled registry, with one preprovisioned pull Secret per OCI namespace. The registry location is operator input. |
| D7 | Base "create" recipe scope | A declarative subset without RUN in v1: FROM a registered digest, layers from approved refs, packages from a pinned repository without maintainer scripts, ENV, numeric non-root USER, WORKDIR, ENTRYPOINT/CMD and LABEL. RUN waits for a rootless builder qualified under restricted pods. |
| D8 | Base lifecycle | `draft → enabled → deprecated(window_end, replacement) → retired`, plus `disabled` with optional `block_invocations`. Deprecated bases cannot be selected by new functions; pinned functions may deploy new revisions on them until `window_end`. Retired bases allow no new revisions, and existing revisions keep serving with a visible flag. Disabled bases allow no new revisions; with `block_invocations`, pinned revisions are denied with `base_disabled`. `block_invocations` is only an explicit, audited superadmin action. Nothing is ever auto-rebased: a replacement can trigger candidate rebuilds that owners promote. The default deprecation window is 90 days (provisional). |
| D9 | Quota model | Shared tenant execution slots plus OCI caps: materialized revisions, warm replicas, pod memory, image bytes and builds. |
| D10 | Invocation state in OCI | Warm reuse within one revision, documented and shown before deploy. Memory, `/tmp` and globals can persist across invocations of the same revision; credentials do not, because each per-attempt handle is invalidated. Single-use pods are deferred. |
| D11 | Cross-mode canary | Explicit opt-in per rollout, default off; otherwise a mode change is a full cutover or rollback. |
| D12 | Signature and scanning | A digest pin is always mandatory. Signature verification is optional per catalog entry, and its result is recorded and shown. Signatures are mandatory for P3 BYO images. Vulnerability scanning is advisory until the operator names an existing scanner. |
| D13 | Builder placement | The image assembler and image intake are separate trusted components in `gntik-ai/falcone-serverless`, rendered by falcone-charts as restricted pods (ADR 0002 amendment). |

**Isolation controls.** OCI pods use PSA restricted (non-root with an arbitrary UID, no privilege escalation, `capabilities.drop: ALL`, seccomp `RuntimeDefault`), `readOnlyRootFilesystem`, an emptyDir `/tmp` with `sizeLimit`, ephemeral-storage, CPU and memory limits, `automountServiceAccountToken: false`, a ServiceAccount with no Role, and `enableServiceLinks: false`. NetworkPolicy allows ingress only from the dispatcher, and egress only to Falcone host APIs, the namespace egress gateway and DNS. `hostUsers: false` and AppArmor `runtime/default` are optional hardening that is recorded as evidence and never assumed. Cancellation asks the container over HTTP first; the bounded kill fallback is a liveness-probe restart that the shim triggers.

**Residual risk accepted per authorization (S2).** A kernel or container-runtime escape compromises the node and every co-scheduled pod. Microarchitectural side channels exist between tenants. Node kernel and runtime patching belongs to the cluster operator, outside Falcone. The authorization flow shows this text, versioned, and the audit record stores the acknowledged version.

**Open prerequisites.** These are qualification inputs (design §5 items 8–12), not reopened decisions: the mirror registry location and pull Secret; whether the kubelet sets `podPidsLimit`; user-namespace and AppArmor availability; SELinux MCS separation on k3s; node patch ownership; an existing scanner; node capacity for up to roughly 400 OCI pods at peak.

## Alternatives

- Wasm only: rejected, because the operator asked for OCI.
- Pod per invocation: deferred (D10); it needs much slower cold paths.
- Direct kubelet pulls from tenant registries: rejected (D6); it needs per-tenant pull Secrets, which the controller may not write.
- An execution credential inside the tenant container: rejected (D5).
- A shared OCI namespace across tenants: rejected (D3).
- Any automatic mode or base switch: rejected.
- RUN-capable builds in v1: rejected (D7).
- A sandbox RuntimeClass: excluded by the deployment-permission requirement (`docs/falcone-serverless-design.md:21`). It remains a possible future hardening tier only where one already exists.

## Consequences

- The `AGENTS.md` hard rules on tenant-supplied artifacts and capability access become mode-scoped, and OCI-mode rules are added.
- OCI isolation is restricted Pod Security on a shared kernel. That is not equivalent to the Wasm boundary, and the residual risk is accepted per authorization.
- New trusted components (OCI shim, egress gateway, image intake, image assembler) need a patch policy and SBOM/provenance (FR-DEP-02).
- Cold-start and density objectives differ by mode. The provisional OCI targets are cold p95 ≤10 s for a node-cached image under 250 MB, and warm platform overhead p95 ≤25 ms. Neither is measured.
- NetworkPolicy enforcement becomes mandatory for OCI mode.
- Knative retirement can proceed for OCI-candidate workloads in authorized projects. Workloads without a candidate verdict in either mode still block retirement.
- OCI and the catalog land in M2 after the Wasm M1 exit. No tenant gets OCI before OCI-07 isolation evidence exists and preflight confirms NetworkPolicy enforcement.
- ADRs 0001, 0002 and 0004–0009 carry "Amended by ADR 0010" notes. ADR 0003 is unaffected, except that Wasm catalog components follow its pin and CVE policy.
