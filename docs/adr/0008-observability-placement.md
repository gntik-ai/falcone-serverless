# ADR 0008: Observability placement

## Status

Proposed — 2026-10-22. Recommendation only; no backend installation or retention policy is approved.

## Context

Design §3 specifies OpenTelemetry instrumentation but leaves backend placement implicit (`docs/falcone-serverless-design.md:246`). Design §2 requires tenant-scoped telemetry and metering (`docs/falcone-serverless-design.md:176-178`). OBS-01 (#42), OBS-02 (#43), OBS-03 (#44) and OPS-01 (#45) need accountable placement, access and operational budgets.

## Decision

Recommend exporting gateway, dispatch, worker lifecycle and host-call telemetry to existing operator-managed observability infrastructure through an approved namespaced collection/export path. Do not add a mandatory cluster-wide collector, operator or tenant-specific backend here. Backend availability, endpoint configuration, capacity and retention would remain platform-operator prerequisites and must be verified; this proposal does not assert an existing compatible deployment.

Recommend that this repository own worker/controller instrumentation, Falcone own authorized tenant-facing retrieval and product metering semantics, and charts own approved deployment/export configuration under ADR 0002. Trusted host/dispatch context would supply tenant/workspace and activation correlation; guest-provided fields would not authorize access. Keep credentials and secret values out of telemetry, bound payload/cardinality and separate operational metrics from auditable billing records. Retention and retrieval isolation require review before enabling a profile.

Recommend that the platform operator specify backend placement, retention, outage buffering/drop behavior and alert routing, with the security approver reviewing scoped access. Telemetry outages should be observable without silently changing durable invocation or charging semantics. Qualification should demonstrate cross-tenant retrieval denial, overload behavior and correlation through host calls.

## Alternatives

- Install a new observability stack with this runtime: expands deployment permissions and operational scope without evidence of need.
- Let guests select export destinations or identity fields: bypasses host scope and egress controls.
- Treat sampled traces as the billing ledger: does not establish durable metering correctness.

## Consequences

Existing infrastructure reuse would reduce runtime deployment scope but create an explicit operational prerequisite. Exact backends, storage budgets, retention and SLOs remain unverified until the platform operator supplies evidence. No instrumentation or external service is changed by this ADR.
