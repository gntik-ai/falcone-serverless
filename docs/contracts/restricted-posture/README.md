# Restricted posture assertion suite v1

Contract: ctr-restricted-posture-suite
Version: 1

This contract provides and consumes the static restricted-pod evidence gate for
SEC-02 criterion 1 and later OCI-05 consumers. It checks one Kubernetes object
represented as JSON, without executing code, rendering charts, contacting a
cluster or proving runtime/SCC or image arbitrary-UID compatibility.

## Inputs and results

`fixtures-v1.json` is an array of `{id, role, expect, object}` cases. IDs are unique;
`role` is `guest-worker` or `controller`; `expect` is `pass` or one rule ID. Every
negative fixture violates exactly one rule. The public Rust `check(object, role)`
returns violations with `rule_id`, once per failed rule, in ascending rule order.
The caller supplies the trusted role; it is not read from tenant object metadata.

RP-12 checks every object first. A disallowed or malformed kind/API version yields
only RP-12. RP-01 through RP-11 apply only to an allowed `apps/v1` Deployment's
`spec.template.spec`. Other allowed kinds need only satisfy RP-12. This is a
posture check, not a complete Kubernetes schema or namespaced RBAC permission
validator; allowed Roles still require independent permission review.

## Rules

Paths below are relative to the Deployment pod spec unless stated otherwise.
Source line numbers refer to snapshot `aec13c3`.

| Rule | Requirement | Source |
| --- | --- | --- |
| RP-01 | Every container's effective `runAsNonRoot` is boolean `true`. Effective `runAsUser` may be absent; if present it is a positive integer, never zero. No fixed UID is required. | [FR-SEC-03, design:159](../../falcone-serverless-design.md#sec-identity-and-security); [AGENTS.md:51](../../../AGENTS.md#cluster-testing-operator-decision-2026-10-22) |
| RP-02 | Every container explicitly sets `securityContext.allowPrivilegeEscalation` to boolean `false`. | [FR-SEC-03, design:159](../../falcone-serverless-design.md#sec-identity-and-security); [ADR 0010:54](../../adr/0010-oci-execution-mode-and-base-catalog.md#decision) |
| RP-03 | Every container's `securityContext.privileged` is absent or boolean `false`. | [SEC-02 criterion 1, design:580](../../falcone-serverless-design.md#sec-identity-and-security-1); [AGENTS.md:23](../../../AGENTS.md#hard-rules-design-6-do-not-relax-without-an-accepted-adr) |
| RP-04 | Every container's `securityContext.capabilities.drop` is a string array containing `ALL`; `capabilities.add` is absent or an empty array. | [ADR 0010:54](../../adr/0010-oci-execution-mode-and-base-catalog.md#decision); [worker runtime contract:74](../worker-runtime-config.md#restricted-pod-posture) |
| RP-05 | Every container's effective `seccompProfile` is an object with `type` exactly `RuntimeDefault`. | [AGENTS.md:51](../../../AGENTS.md#cluster-testing-operator-decision-2026-10-22); [ADR 0010:54](../../adr/0010-oci-execution-mode-and-base-catalog.md#decision) |
| RP-06 | Every container explicitly sets `securityContext.readOnlyRootFilesystem` to boolean `true`. | [AGENTS.md:51](../../../AGENTS.md#cluster-testing-operator-decision-2026-10-22); [worker runtime contract:71](../worker-runtime-config.md#restricted-pod-posture) |
| RP-07 | `hostNetwork`, `hostPID` and `hostIPC` are each absent or boolean `false`. | [FR-SEC-03, design:159](../../falcone-serverless-design.md#sec-identity-and-security); [deployment permissions, design:23](../../falcone-serverless-design.md#confirmed-requirements) |
| RP-08 | No volume contains a `hostPath` field, regardless of its value. | [FR-SEC-03, design:159](../../falcone-serverless-design.md#sec-identity-and-security); [AGENTS.md:23](../../../AGENTS.md#hard-rules-design-6-do-not-relax-without-an-accepted-adr) |
| RP-09 | No container port contains a `hostPort` field, regardless of its value. | [FR-SEC-03, design:159](../../falcone-serverless-design.md#sec-identity-and-security); [SEC-02 criterion 1, design:580](../../falcone-serverless-design.md#sec-identity-and-security-1) |
| RP-10 | For `guest-worker`, `automountServiceAccountToken` is explicitly boolean `false` on the pod spec. For `controller`, this rule does not apply: the controller needs API access. | [FR-SEC-03, design:159](../../falcone-serverless-design.md#sec-identity-and-security); [controller write set, AGENTS.md:23](../../../AGENTS.md#hard-rules-design-6-do-not-relax-without-an-accepted-adr) |
| RP-11 | `runtimeClassName` is absent, regardless of value. | [deployment permissions, design:23](../../falcone-serverless-design.md#confirmed-requirements); [FR-SEC-04, design:160](../../falcone-serverless-design.md#sec-identity-and-security) |
| RP-12 | Strict object allow-list: `apps/v1` Deployment; `v1` Service or ServiceAccount; `coordination.k8s.io/v1` Lease; `rbac.authorization.k8s.io/v1` Role or RoleBinding. A RoleBinding additionally requires `roleRef` to be an object with `kind` exactly `Role`. All other kinds or API versions, including missing kind, ClusterRole, CRD, SCC and ConfigMap, fail. | [deployment permissions, design:23](../../falcone-serverless-design.md#confirmed-requirements); [SEC-02 criterion 1, design:580](../../falcone-serverless-design.md#sec-identity-and-security-1); [AGENTS.md:23](../../../AGENTS.md#hard-rules-design-6-do-not-relax-without-an-accepted-adr) |

## Effective values and malformed input

Every regular container and every initContainer is checked, not just the first.
For RP-01 and RP-05, each field present in the container `securityContext`
overrides that field in the pod `securityContext`, even if the overriding value
is null or wrongly typed. `seccompProfile` is replaced as a whole, never merged.
RP-02, RP-03, RP-04 and RP-06 read only container security contexts; pod values
cannot satisfy those requirements.

Missing required fields fail closed. Optional absent fields are permitted only
where a rule explicitly says so; null and wrongly typed values are not absence.
No malformed input may panic or silently produce a pass. A malformed pod spec
fails every applicable pod rule. A missing, empty or malformed `containers` list,
or a malformed `initContainers` list or container entry, fails RP-01 through
RP-06 and RP-09. Absent or empty `initContainers` is valid. A malformed pod
security context fails the effective-value rules RP-01 and RP-05; a malformed
container security context fails RP-01 through RP-06. Malformed volume lists or
entries fail RP-08; malformed port lists or entries fail RP-09. Unrelated metadata
and Kubernetes fields outside this contract are not schema-validated.

The controls agree with the [worker runtime contract:63–78](../worker-runtime-config.md#restricted-pod-posture).
OCI-only controls (emptyDir size limits, resources, enableServiceLinks,
NetworkPolicy, hostUsers and AppArmor), chart rendering, YAML parsing, runtime
qualification and SEC-02 criteria 2 and 3 are outside v1.

## Versioning

After merge, v1 and `fixtures-v1.json` are frozen. Corrections must ship as a new
versioned specification and fixture file; consumers pin their version explicitly.
Before any consumer pins v1, rollback may revert the merge. Once pinned, publish
a corrected version and preserve v1.
