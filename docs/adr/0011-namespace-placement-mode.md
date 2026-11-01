# ADR 0011: Namespace placement mode

## Status

Status: Accepted — 2026-11-01 (proposed 2026-10-30). **R3 (tenant isolation).** Operator decision: namespace creation stays available and is switchable, either by creating a dedicated namespace (create mode) or by using a shared, preprovisioned namespace; the defaults `namespaceMode=preprovisioned` and `testNamespaceMode=create` are unchanged. Acceptance records that decision only. It does not by itself relax any hard rule: the operator must still apply the AGENTS.md amendments below (makers never edit that protected file), and the independent second review and operator approval that R3 requires remain conditions of any ChangeSet that implements a mode. This docs-only ChangeSet neither implements the modes nor claims their enforcement is qualified.

## Context

The baseline at `45d6277` assumes administrator-provided namespaces: FR-TEN-02 and TEN-01, FR-OPS-01 and OPS-01, FR-OCI-05, design §3, and AGENTS.md:23 and :31. AGENTS.md:49 requires runtime tests to create and delete dedicated `fs-test-*` namespaces. An identity limited to its release namespace may lack both dedicated runtime namespaces and permission to create test namespaces.

The operator wants explicit choices for runtime placement and test fixture placement. These are separate controls: a workload placement choice cannot infer test placement or authorize test access. The defaults remain `namespaceMode=preprovisioned` and `testNamespaceMode=create`.

Shared runtime placement changes tenant isolation. At this baseline FR-TEN-02 explicitly refuses labels as a security boundary; Proposed ADR 0007 recommends tenant/profile pools in dedicated, preprovisioned namespaces. Accepted ADR 0010 requires a dedicated, preprovisioned OCI namespace per tenant and rejects shared OCI namespaces. This proposal would amend those placement rules only after acceptance and the operator hard-rule amendment. ADRs 0001, 0007 and 0010 remain unchanged here.

## Decision

### Runtime placement: namespaceMode

`namespaceMode` has exactly three values. It controls only serverless runtime placement: worker pools, controller-rendered Deployments and Services, OCI revision pods and the egress gateway. It does not control test fixtures, Falcone's own namespaces, or arbitrary platform services.

| Value | Placement and authority |
| --- | --- |
| `preprovisioned` (default) | Operator-owned bindings resolve to administrator-provided dedicated pool namespaces and a dedicated OCI namespace per tenant. No namespace creation is requested. Required namespaced RBAC must already be granted. This preserves current behavior. |
| `create` (proposed) | The installer creates dedicated pool and per-tenant OCI namespaces and their namespaced RBAC only after preflight, using the actual deploying identity, proves permission to create namespaces and the required RBAC. Denial or inability to prove permission fails closed with an error naming `namespaceMode`, for example `namespaceMode=create: namespace/RBAC creation permission not proven`. It never silently falls back to shared or preprovisioned placement. |
| `shared` (proposed) | Bindings resolve the listed runtime workloads to the release namespace. No namespace creation is requested. Every tenant and revision retains distinct resource ownership and isolation controls within that namespace. |

The proposed `create` exception is limited to dedicated Namespace creation and namespaced RBAC provisioning by the installer. Whether the Namespace is a chart-rendered resource or an installer step remains an operator decision before implementation. Neither route grants permissions to itself. No mode requires an operator, CRD, ClusterRole, custom RuntimeClass, elevated SCC, host access, or admission configuration change. Restricted pods remain mandatory. The controller's write set remains standard Deployments, Services and Leases in the resolved, authorized namespaces; it never gains Namespace or RBAC writes, or Pod, Job or Secret writes.

All modes need preflight proof of the required namespaced verbs, correct bindings and applicable network enforcement. Unknown or unauthorized bindings fail before Kubernetes writes. The intake-mirror pull Secret must already be provisioned in each resolved OCI namespace; the controller does not create it. Changing namespaceMode is not an authorization grant and never bypasses these checks.

### Shared runtime ownership and isolation

Proposed shared mode requires all of the following:

- Per-tenant names and ownership labels for pools and related resources; OCI resources additionally have per-revision names and labels. Identical function names in different tenants must not collide. Reconciliation and deletion verify the bound tenant/revision ownership.
- Namespaced RBAC with a distinct service account per tenant and least-privilege Roles/RoleBindings. Tenant code receives no automatic Kubernetes API token. Controller authority stays separate and namespaced.
- Tenant-scoped NetworkPolicies covering ingress and egress, with verified enforcement and selectors that distinguish tenants even within the same namespace. Namespace-only selectors cannot establish the shared-mode tenant boundary. Direct cross-tenant traffic is denied.
- Existing verified pool mappings, scoped execution credentials, host binding checks and SQL/RLS enforcement. Labels are not API authorization and cannot replace these checks.

**Labels and NetworkPolicies become the tenant boundary in shared mode, contrary to FR-TEN-02 at the baseline. This is an R3 weakening of namespace isolation.** Wasm still runs through the restricted worker and versioned capability host; no tenant native artifact or precompiled Wasmtime artifact is loaded as a workaround. No shared-mode capability is active merely because this proposal is documented.

OCI additionally shares the node kernel. In proposed shared mode it is unavailable unless the platform superadmin explicitly enables **“OCI in shared namespace” per tenant**, in addition to the existing project OCI authorization. Admission must require that enablement and verified tenant-scoped NetworkPolicy enforcement. Missing either blocks OCI; it never degrades isolation. The execution credential stays in the trusted shim, capability access remains through the shim and egress gateway, and the existing credential/RLS and reviewed secret-grant rules continue to apply. Gateway placement follows namespaceMode; whether shared mode uses one gateway or a gateway per tenant remains open, and either topology must enforce tenant scope.

### Test placement: testNamespaceMode

`testNamespaceMode` has exactly two values and is the **only control for test fixture placement**, independent of namespaceMode. Every combination is independent and valid as a placement choice, subject to authorization and prerequisites. It never inherits a value from namespaceMode. falcone-serverless #121 is the test harness entry point; implementation and migration of tests are separate work.

| Value | Placement, ownership and cleanup |
| --- | --- |
| `create` (default) | The test creates and deletes its dedicated `fs-test-<run>` namespace on the shared cluster, using a unique run identity. This preserves today's test behavior. An identity that cannot create the namespace must fail closed with an error naming `testNamespaceMode`; it must not silently switch modes. |
| `shared` (proposed) | The test uses the explicitly configured, existing `FS_TEST_NAMESPACE`. Every fixture has a per-run name/suffix and `falcone.test/run=<nonce>` label owned by that run. Setup refuses collisions with resources it did not label. Cleanup selects only that exact run label and verifies ownership before deletion; it never deletes the namespace. Tests refuse to mutate, adopt or delete unlabelled resources or resources labelled for another run, even on failure. |

For example:

- `namespaceMode=shared` with `testNamespaceMode=create`: runtime workloads use the release namespace, while tests create and delete `fs-test-<run>`.
- `namespaceMode=create` with `testNamespaceMode=shared`: dedicated runtime namespaces require preflight authority, while tests use `FS_TEST_NAMESPACE` with owned per-run fixtures.
- `namespaceMode=preprovisioned` with `testNamespaceMode=shared`: runtime bindings use administrator-provided namespaces, while tests use `FS_TEST_NAMESPACE` with the same run-label protections.

Test pods always meet Pod Security restricted, seccomp `RuntimeDefault`, a read-only root filesystem and an arbitrary non-root UID. Missing FS_TEST_NAMESPACE, insufficient namespaced permissions or unavailable required enforcement fails closed. Shared test placement requires explicit operator authorization of that existing test namespace after the hard-rule amendment; knowing a namespace name is not authority to use it.

The off-limits namespaces are unchanged in every combination. Only integration tests may read `in-falcone-staging`; they may not write fixtures there. Every other namespace remains off limits, including `family-rentals-prod`, `musematic*` and `llmwiki*`, apart from the dedicated or explicitly authorized shared test namespace. Setting FS_TEST_NAMESPACE to an off-limits namespace does not authorize access. No credentials are printed or committed; credentials continue to come from OpenBao through the existing External Secrets setup.

### Operator-only AGENTS.md amendment

These are proposed replacement words for the baseline lines, supplied for the operator to apply after acceptance. AGENTS.md:21 requires an accepted ADR before a hard rule is relaxed. This ChangeSet does not edit AGENTS.md; operator application is a rollout/completion gate, not a maker deliverable. Retain the other hard rules and cluster-testing bullets.

**Replace AGENTS.md:23:**

> Restricted pods only, in both runtime modes: no operator, CRD, ClusterRole, custom RuntimeClass, elevated SCC or host access. Runtime namespace placement follows accepted ADR 0011: namespaceMode=preprovisioned by default uses administrator-provided bindings; create permits installer creation of dedicated pool/OCI namespaces and namespaced RBAC only after preflight proves the deploying identity's authority, failing closed with an error naming namespaceMode on denial; shared resolves pools, controller-rendered Deployments/Services, OCI revision pods and the egress gateway to the release namespace. Shared requires per-tenant/per-revision names and labels, namespaced RBAC, distinct tenant service accounts and tenant-scoped NetworkPolicies, retaining credential/RLS checks. Labels and NetworkPolicies are the shared-mode tenant boundary (R3). The controller writes only standard Deployments, Services and Leases in the resolved authorized namespaces; it never creates namespaces or RBAC.

**Replace AGENTS.md:31:**

> OCI pods run single-revision under the restricted posture in the tenant's bound OCI namespace: dedicated and administrator-provided in namespaceMode=preprovisioned (default), dedicated and installer-created after namespace/RBAC permission preflight in create, or the release namespace in shared (ADR 0011). Shared OCI additionally requires explicit per-tenant superadmin enablement of “OCI in shared namespace” and verified tenant-scoped NetworkPolicy enforcement; otherwise OCI is blocked. Existing project OCI authorization and credential/RLS checks still apply. The controller's write set is unchanged.

**Replace AGENTS.md:49:**

> Every runtime test runs on the shared cluster; test fixture placement depends only on testNamespaceMode, independently of namespaceMode (ADR 0011; #121). create (default) creates and deletes a dedicated fs-test-<run> namespace and fails closed naming testNamespaceMode if creation is unauthorized. shared uses an explicitly operator-authorized existing FS_TEST_NAMESPACE, per-run resource names and falcone.test/run=<nonce>; it cleans up only resources bearing its own exact run label, verifies ownership, refuses to touch unlabelled or other-run resources and never deletes the namespace. All existing off-limits namespace restrictions and restricted test-pod requirements remain in force.

### Published issue amendment plan

All issue numbers and **open** statuses below are operator-supplied and **not verified in this snapshot**. This table records needed amendments, not changes to GitHub issue bodies, charts or Falcone. Where a listed task includes cluster tests, test placement follows #121 and the Hermes maker rule independently of runtime placement. No unverified task title or implementation status is inferred.

| Repository | Issue | Status | Needed change |
| --- | --- | --- | --- |
| falcone-charts | #88 | open | Render the scoped runtime resources to the release namespace in shared, otherwise dedicated namespaces; retain preprovisioned as default and preflight create authority. |
| falcone-charts | #89 | open | Render the scoped runtime resources to the release namespace in shared, otherwise dedicated namespaces; preserve namespaced RBAC and tenant ownership. |
| falcone-charts | #90 | open | Render the scoped runtime resources to the release namespace in shared, otherwise dedicated namespaces; use per-tenant/per-revision names and labels. |
| falcone-charts | #93 | open | Render the scoped runtime resources to the release namespace in shared, otherwise dedicated namespaces; require distinct tenant service accounts. |
| falcone-charts | #94 | open | Render the scoped runtime resources to the release namespace in shared, otherwise dedicated namespaces; project tenant-scoped NetworkPolicies. |
| falcone-charts | #95 | open | Render the scoped runtime resources to the release namespace in shared, otherwise dedicated namespaces; verify shared tenant selectors and enforcement. |
| falcone-charts | #98 | open | Render the scoped runtime resources to the release namespace in shared, otherwise dedicated namespaces; resolve OCI bindings and pull-Secret references by mode. |
| falcone-charts | #99 | open | Render the scoped runtime resources to the release namespace in shared, otherwise dedicated namespaces; retain the shared OCI superadmin/enforcement gates. |
| falcone-charts | #100 | open | Render the scoped runtime resources to the release namespace in shared, otherwise dedicated namespaces; resolve egress gateway placement and tenant scope. |
| falcone | #1319 | open | Add the runtime namespaceMode contract, default preprovisioned, with operator-owned binding resolution and create/shared prerequisites. |
| falcone | #1282 | open | Make OCI namespace resolution mode-aware; shared requires per-tenant superadmin enablement and verified enforcement. |
| falcone | #1412 | open | Make OCI namespace resolution mode-aware; shared requires per-tenant superadmin enablement and verified enforcement. |
| falcone | #1427 | open | Make OCI namespace resolution mode-aware; shared requires per-tenant superadmin enablement and verified enforcement. |
| falcone | #1479 | open | Make OCI namespace resolution mode-aware; shared requires per-tenant superadmin enablement and verified enforcement. |
| falcone-serverless | #36 | open | Amend namespace bindings for all runtime modes, retaining verified tenant mappings and shared OCI gates. |
| falcone-serverless | #45 | open | Amend installation/preflight criteria for default preprovisioned, proposed shared and the explicit Namespace-only cluster-scoped create exception. |
| falcone-serverless | #63 | open | Amend applicable namespace assumptions to resolve runtime placement by namespaceMode and cluster fixtures independently by testNamespaceMode/#121. |
| falcone-serverless | #76 | open | Amend applicable namespace assumptions to resolve runtime placement by namespaceMode and cluster fixtures independently by testNamespaceMode/#121. |
| falcone-serverless | #107 | open | Amend applicable namespace assumptions to resolve runtime placement by namespaceMode and cluster fixtures independently by testNamespaceMode/#121. |
| falcone-serverless | #110 | open | Amend applicable namespace assumptions to resolve runtime placement by namespaceMode and cluster fixtures independently by testNamespaceMode/#121. |
| falcone-serverless | #119 | open | Amend applicable namespace assumptions to resolve runtime placement by namespaceMode and cluster fixtures independently by testNamespaceMode/#121. |
| falcone-serverless | #172 | open | Amend applicable namespace assumptions to resolve runtime placement by namespaceMode and cluster fixtures independently by testNamespaceMode/#121. |
| falcone-serverless | Other cluster-test issues (~85; operator estimate) | open | Follow #121 and the Hermes maker rule: use only the explicitly authorized fixture placement, per-run ownership/label cleanup and unchanged off-limits boundaries; makers never edit protected agent configuration, deploy or infer missing authority. |

### Open implementation questions

- Where do namespaceMode and testNamespaceMode live, and can either change after installation? No live migration behavior is implied here.
- In create mode, does the chart render Namespace resources or does an installer step create them? FR-OPS-01 records only the proposed dedicated Namespace exception, not a chosen mechanism.
- Does shared runtime placement use one egress gateway or one per tenant? Either must retain tenant-scoped enforcement.
- Should ADRs 0001, 0007 and 0010 later receive cross-reference notes? They are not rewritten here.

## Alternatives

- Keep only preprovisioned runtime namespaces: preserves the current tenant namespace boundary but cannot serve an installation authorized for only its release namespace.
- Always create namespaces: rejected as a universal requirement because the deploying identity may lack authority; creation must be an explicit mode with fail-closed preflight.
- Always share the release namespace: rejected as a default because it weakens tenant isolation and existing installations retain preprovisioned placement.
- Use one switch for runtime and test namespaces: rejected because their permissions, lifecycle and ownership differ. The cross-combinations above must remain possible.
- Fall back automatically after permission denial, use labels without enforced NetworkPolicies, or enable shared OCI without a separate tenant gate: rejected because they silently weaken isolation.

## Consequences

- This is an R3 proposal requiring independent second review, operator approval, ADR acceptance and the operator-applied AGENTS.md wording before hard rules change. Documentation does not activate shared/create support.
- Default namespaceMode=preprovisioned and testNamespaceMode=create preserve today's placement. The two settings remain independent.
- Shared runtime mode exchanges a dedicated namespace boundary for label-selected, tenant-scoped NetworkPolicy enforcement, distinct service accounts and namespaced RBAC. It needs adversarial evidence, including same-namespace cross-tenant denial; OCI retains same-kernel residual risk and its extra per-tenant superadmin gate.
- Separate code, chart and harness issues must implement permission preflight, mode-aware bindings, shared ownership, enforcement verification and safe cleanup. This ChangeSet implements none of them, including #121, and makes no runtime qualification claim.
- Requirements and stories are reworded without changing their counts; generated design/backlog files come only from docs/build_package.py. The historical docs/architecture-review.md remains pinned and unchanged.
- Issue amendments are a separate operator action. Falcone's own namespaces, Falcone code and chart implementation are outside this source ChangeSet.
- Rollback is reverting the docs commit or marking this ADR Rejected/Superseded. No runtime component reads these files; the documented defaults remain unchanged.
