# ADR 0004: Per-invocation isolation

## Status

Proposed — 2026-10-22. Acceptance requires **R3 review: a second reviewer and explicit operator approval** (`AGENTS.md:24`). The Decision below is a recommendation pending that review, not accepted policy.

## Context

Design §3 recommends a fresh trusted execution process where feasible while requiring fresh guest state (`docs/falcone-serverless-design.md:287-289`). Its provisional warm p95 overhead target is ≤20 ms and cold p95 target ≤5 s (`docs/falcone-serverless-design.md:353-354`). Neither is measured. RUN-03 (#20), SEC-02 (#34), WAS-01 (#51) and WAS-03 (#53) require lifecycle, sandbox and leakage evidence.

## Decision

Recommend a fresh, fixed trusted execution process per invocation as the first qualification candidate, inside the existing restricted worker pod. Each invocation still receives a fresh Wasm instance and execution-scoped capability handles. The process boundary supplements Wasm validation; it does not enable guest process creation, arbitrary host plugins or native tenant code. Reuse only trusted code caches, never dirty guest memory, handles, credentials or temporary files.

Recommend measuring startup, compilation, harness creation and host-call cancellation separately against both provisional latency objectives. Demonstrate isolation across sequential and concurrent invocations, including failures, cancellation and cleanup. Guest fuel/epoch interruption alone does not bound blocked host calls; retain independent host deadlines and a bounded trusted-process termination path. A terminated call may have external effects and is not safe to replay blindly.

Recommend considering in-process fresh instances only as a separately reviewed candidate after evidence covers cross-invocation leakage, crash containment and cancellation. Missing latency targets should produce an explicit qualification finding, not an implicit switch to multiplexing or weaker isolation. The security approver and runtime maintainer would supply comparative evidence for the R3 decision.

## Alternatives

- Fresh instances in a shared process: potentially lower startup overhead, but shared host state and crash/cancellation effects need stronger evidence.
- One pod per invocation: stronger lifecycle separation with additional pod startup and control-plane costs to measure; does not remove host security obligations.
- Reuse dirty guest instances: excluded by design §6 (`docs/falcone-serverless-design.md:724`).

## Consequences

The recommendation may miss latency targets and consume extra process resources. No containment or performance result is claimed. The existing fresh-state rule remains in force; process strategy stays proposed until qualification and R3 approval.
