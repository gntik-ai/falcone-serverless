# ADR 0009: Schema and migration ownership

## Status

Proposed — 2026-10-22. Recommendation only; no database or schema migration is authorized.

## Context

Design §3 reuses PostgreSQL and makes it authoritative for Function/Revision/Trigger/Job records (`docs/falcone-serverless-design.md:260,275`). It does not specify migration ownership. Design §2 requires scope-aware durable state and outbox/inbox fencing (`docs/falcone-serverless-design.md:173-174`). STO-03 (#41), DEP-03 (#23), OPS-03 (#47) and MIG-01 (#48) need one authority and upgrade/restore compatibility.

## Decision

Recommend retaining authoritative function, revision, trigger, job, activation and outbox/inbox state in Falcone's existing PostgreSQL database, rather than introducing a separately authoritative runtime database. The Falcone maintainer would own schema definitions, tenant/workspace constraints, transactional publication and migrations in `gntik-ai/falcone`, consistent with ADR 0002. This is a recommendation about ownership, not evidence that the required schemas already exist.

Recommend that the worker/controller use reviewed, versioned Falcone contracts for dispatch and desired-state projections. They would not perform independent DDL, embed product-policy migrations or create a second source of truth. Exact transport/read contracts remain to be specified by the owning maintainers; projections and replica counts must not overwrite authoritative generations or terminal attempt state.

Recommend requiring expand/contract compatibility, fenced attempt ownership, scoped uniqueness and restore/rollback evidence before schema changes are enabled. The Falcone maintainer would coordinate application/schema versions; the platform operator would own backups and restore execution. Refuse an unsafe downgrade instead of silently discarding accepted work. Existing database permissions and migration facilities remain unverified and require preflight; missing Falcone facilities should be reported in that repository, not worked around locally.

## Alternatives

- Add a runtime-owned authoritative PostgreSQL database: introduces cross-database consistency, backup and migration obligations without a demonstrated requirement.
- Let both controller and Falcone migrate shared tables: creates competing ownership and unsafe upgrade order.
- Treat Kubernetes resources as the durable authority: conflicts with the PostgreSQL/outbox design and namespace-only projections.

## Consequences

Acceptance would assign one schema authority and require versioned cross-repository contracts. Runtime delivery would depend on coordinated Falcone migrations and restore evidence. No schema is inspected, created or changed here, and MIG-01's contract inventory and dependency classification remain outstanding.
