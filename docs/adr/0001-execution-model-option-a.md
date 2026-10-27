# ADR 0001: Execution model — Option A

## Status

Accepted — operator, 2026-10-21. This records the existing direction, not a runtime qualification.

## Context

The accepted direction and confirmed requirements in `docs/falcone-serverless-design.md:3-31` separate deployed definitions and messaging from execution capacity. Design §3 (`docs/falcone-serverless-design.md:248-287`) describes the worker and controller; §6 (`docs/falcone-serverless-design.md:714-724`) excludes mandatory cluster extensions and native tenant workloads.

MIG-01 (#48), criterion (2), requires these facts to be recorded (`docs/backlog.md:257`). WAS-01 (#51) depends on MIG-01 (`docs/backlog.md:276-280`).

## Decision

Record Option A: Wasmtime/WASI inside ordinary restricted Kubernetes/OpenShift pods, without an administrator-provided sandbox runtime (no admin sandbox). There is no required operator, CRD, ClusterRole, custom RuntimeClass, elevated SCC or host access. Use preprovisioned namespaces and explicitly granted namespaced permissions.

The workload assumptions are **20 tenants**, **10,000 deployed functions**, and **100,000 mostly WebSocket/event messages per second**. Messages are not synonymous with function invocations; deployed functions do not each allocate a pod. **Falcone owns quotas**, accounting and charging, as well as public APIs, authorization, admission/dispatch, execution policy, integrations, routing and lifecycle.

A Rust execution worker embeds Wasmtime. A Go namespaced pool controller writes only Deployments, Services and Leases, with exclusive replica-count ownership. Guests reach the platform only through versioned WIT/WASI capabilities. No arbitrary tenant native execution, native fallback or tenant-produced precompiled artifact deserialization is permitted; trusted compilation starts from validated Wasm.

Implementation, restricted-cluster feasibility and security qualification remain **unverified until WAS-01 (#51)** supplies the relevant evidence; later host/profile security gates still apply. This ADR satisfies only MIG-01 criterion (2). Contract inventory (1) and dependency classification (3) remain open and gate MIG-01 and, through it, WAS-01. Recording the decision does not authorize implementation past those gates.

## Alternatives

- Retain existing Knative as a migration reference where already installed; it is not a prerequisite for a clean Option A installation.
- Arbitrary tenant containers/native binaries or administrator-installed sandbox runtimes do not meet the accepted constraints.
- External execution pools are not an automatic fallback for incompatible workloads.

## Consequences

Compatibility must be demonstrated for each qualified language/package profile; unsupported dependencies block migration. Startup cost, capacity and hostile-code safety need measurement and independent qualification. Accepting an architecture direction is not evidence that the implementation or performance targets work. ADRs 0003–0009 remain proposals; ADR 0002 records repository ownership separately.

## Amended by ADR 0010

2026-10-27. Superseded in part by [ADR 0010](0010-oci-execution-mode-and-base-catalog.md) for `runtime.mode=oci`. The Option A decision remains in force for Wasm, which is the default mode. ADR 0010 adds an explicit OCI mode that the platform superadmin authorizes per tenant and project. It is never a fallback, and the controller's write set is unchanged.

Line citations above to the design and `AGENTS.md` refer to their versions before ADR 0010 (commit `d459340`); the ADR 0010 revision shifts them.
