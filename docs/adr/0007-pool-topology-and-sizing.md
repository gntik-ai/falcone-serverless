# ADR 0007: Pool topology and sizing

## Status

Proposed — 2026-10-22. Recommendation only; quotas and measured capacity are not changed.

## Context

Design §3 starts with tenant/profile-specific pools (`docs/falcone-serverless-design.md:287`) and distinguishes message, dispatch and execution scaling (`docs/falcone-serverless-design.md:322-324`). The initial quota estimate is 20 executions per tenant, approximately 400 total (`docs/falcone-serverless-design.md:23,31`). INV-02 (#25), TEN-02 (#37), DEP-03 (#23) and JOB-03 (#56) cover admission, scaling and mixed-load evidence.

## Decision

Recommend retaining tenant/profile-specific pools in preprovisioned namespaces, with separate logical interactive and analysis reservations inside each tenant's Falcone-owned quota. A profile's pods/capacity would be shared across that tenant's compatible function revisions; 10,000 definitions would not imply 10,000 pods or Services. Pool identity mappings must be verified; labels are not authorization. Any later change to tenant isolation requires the security review in `AGENTS.md:24`.

Recommend interpreting **400 versus 1,000 slots as a quota-versus-demand mismatch**, not two promises of capacity. At 100,000 messages/s, a 1% trigger fraction gives 1,000 invocations/s. With a one-second mean execution time, approximately 1,000 executions would be active before headroom. A 400-slot aggregate quota cannot sustain that admitted workload: it must queue within bounded limits or reject excess work. Increasing worker replicas cannot bypass Falcone quotas. Neither number is a measured hardware sizing result.

Recommend retaining the 20-per-tenant starting assumption for qualification until the Falcone product owner and platform operator approve quota/capacity changes from measured trigger fraction, fan-out, duration, CPU/memory and burst distributions. Count messages, connections, deliveries and executions separately. Reserve interactive/analysis budgets so long jobs cannot monopolize capacity; account for checkpoints and draining. The namespaced pool controller remains the exclusive replica-count writer; desired-state reconciliation must not reset its scaling decisions.

## Alternatives

- One pool per function: unnecessarily ties inventory to pod count and idle cost.
- One shared cross-tenant pool: introduces an isolation change outside this proposal.
- Declare 1,000 slots provisioned or raise quotas to hide overload: unsupported by measurements and outside runtime ownership.

## Consequences

This interpretation removes the apparent numerical conflict without promising 100,000 guest invocations/s. Operators still need workload evidence to size replicas and memory. Quota exhaustion and queueing must be reported honestly in benchmarks; all latency/throughput claims remain provisional.
