# ADR 0006: Pool identity and OpenBao

## Status

Proposed — 2026-10-22. Acceptance requires **R3 review: a second reviewer and explicit operator approval** (`AGENTS.md:24`). The Decision is a recommendation pending R3 review; no credential flow is implemented or certified.

## Context

Design §2 requires scoped OpenBao secret resolution (`docs/falcone-serverless-design.md:172`). Design §3 separates worker/controller access and keeps credentials in host bindings (`docs/falcone-serverless-design.md:271,301`). SEC-03 (#35), TEN-01 (#36), STO-02 (#40) and WAS-02 (#52) need an issuer and verifiable pool-to-tenant mapping. Existing credential delivery uses OpenBao through External Secrets (`AGENTS.md:42`); its authentication configuration has not been inspected here.

## Decision

Recommend reusing the existing OpenBao/External Secrets path: pools would not log in to OpenBao directly. The platform operator would own the External Secrets integration's authenticated OpenBao identity, narrow policy and tenant/pool-scoped delivery. Trusted host bindings would consume only the scoped material supplied through that integration. Authentication method, isolation of delivered material, refresh and revocation behavior need operator evidence before acceptance; this ADR neither invents a deployed OpenBao auth method nor authorizes replacement of the existing integration.

Recommend that Falcone admission/dispatch issue a short-lived execution credential after authorization and quota reservation. It would bind trusted tenant/workspace, pool/profile, revision, activation/attempt, audience, expiry and allowed capabilities. The trusted host would verify issuer, scope, audience, expiry and attempt ownership before resolving a secret reference. Guest fields or pool labels alone would never establish authority. The exact credential format, signing/verification contract and replay protection remain review deliverables owned by the Falcone maintainer, not new code in this repository.

Recommend invalidating execution handles on completion, cancellation or expiry and testing stale/wrong-pool credentials, rotation and revocation before qualification. Guest worker service accounts retain no automatic Kubernetes API token. Secret values remain in host bindings, outside guest environment, files and logs, absent a separately reviewed narrow guest grant. Controller identity remains separate and namespaced. No credential values or actual deployment identifiers are needed for the review.

## Alternatives

- Direct pool-to-OpenBao workload authentication: would need a separately reviewed host-only identity and token-delivery mechanism; it must not silently replace External Secrets.
- Shared long-lived pool credentials or guest-supplied scope: weakens rotation and tenant isolation.
- Let the controller issue execution grants: confuses infrastructure reconciliation with Falcone authorization and admission ownership.

## Consequences

This recommends an issuer and authentication responsibility, but leaves the deployed integration unverified. The platform operator owns delivery evidence, the Falcone maintainer owns the execution-credential contract, and the security approver reviews cross-tenant denial and revocation. A missing integration capability is a qualification finding, not permission to expose secrets to guests.
