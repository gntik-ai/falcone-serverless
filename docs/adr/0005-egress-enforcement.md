# ADR 0005: Egress enforcement

## Status

Proposed — 2026-10-22. Acceptance requires **R3 review: a second reviewer and explicit operator approval** (`AGENTS.md:24`). The Decision is a recommendation pending R3 review.

## Context

Design §3 requires host allow-listed HTTP (`docs/falcone-serverless-design.md:296`), permission preflight and corporate-owned network enforcement (`docs/falcone-serverless-design.md:271-273`). SEC-03 (#35), WAS-02 (#52) and OPS-01 (#45) need a boundary that neither grants raw guest sockets nor assumes unavailable NetworkPolicy authority.

## Decision

Recommend two complementary controls. The versioned WIT capability host would enforce verified invocation grants and an explicit outbound HTTP allow-list, bounded request/response sizes and deadlines. Destination validation should cover DNS resolution, every redirect and rebinding attempts, denying destinations outside the grant, including internal infrastructure endpoints unless explicitly authorized. Guests retain no direct filesystem, environment, raw-network or infrastructure-credential access.

Recommend that the platform operator own preprovisioning and enforcement of namespace NetworkPolicies, with reviewed chart projections owned in `gntik-ai/falcone-charts` where authorized. The controller here remains limited to Deployments, Services and Leases; it does not create NetworkPolicies or broaden RBAC. Network rules would allow only the trusted host's required platform paths and approved egress path. A shared worker pod's network identity cannot express per-invocation grants; the host allow-list remains necessary.

Recommend preflight evidence for both namespaced permissions and actual deny/allow behavior under the installed network implementation. Missing permissions or ineffective enforcement should block qualification instead of disabling controls. The platform operator owns enforcement evidence; the security approver reviews bypass attempts and the Falcone maintainer owns grant policy. No network access or policy change is performed by this ADR.

## Alternatives

- Host allow-list alone: leaves host-compromise containment dependent on one layer.
- NetworkPolicy alone: cannot authorize tenant/workspace-specific HTTP operations or validate redirects at the invocation boundary.
- Let the pool controller own policy objects: conflicts with its accepted write boundary.

## Consequences

The proposed deployment needs verified corporate network prerequisites and may constrain allowable destinations. Egress failures must be diagnosable without exposing credentials. Acceptance would require joint host and network evidence; neither layer's effectiveness is asserted here.

## Amended by ADR 0010

2026-10-27. For OCI mode ([ADR 0010](0010-oci-execution-mode-and-base-catalog.md)), NetworkPolicy is a *primary* control, not a supplement. OCI pods egress only to Falcone host APIs, the per-namespace egress gateway and DNS. Allow-listed HTTP goes through the shim to the egress gateway, which applies the host-layer checks. If preflight cannot confirm NetworkPolicy enforcement, OCI mode is blocked. This note does not change this ADR's Proposed status or its R3 review requirement.

Line citations above to the design and `AGENTS.md` refer to their versions before ADR 0010 (commit `d459340`); the ADR 0010 revision shifts them.
