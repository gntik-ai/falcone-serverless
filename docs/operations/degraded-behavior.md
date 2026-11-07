# Pool controller degraded behavior

The controller campaigns only after the read-only preflight passes. Election is
always enabled, including with one replica. The only election object is
`falcone-pool-controller-leader` in `POD_NAMESPACE`, injected by the downward API
and required to equal `controllerNamespace`. No pool namespace or scaler Lease
is used. Existing objects must satisfy ctr-resource-ownership v1; otherwise the
controller logs `leader lease ownership unverified`, preserves the object and
remains a standby. Operator repair must establish ownership independently.

Production pool reconciliation, replica changes, deletion and Falcone publishing
remain no-ops until DEP-03. The leader package supplies the fence, not a desired
state client, scaler, report emitter or observationSeq store. Future duties must
check their acquisition's Gate immediately before every write or publish, use its
epoch as reporterEpoch, and stop when its context is cancelled. Holding this Lease
never authorizes the separate scaler duty or relaxes pool ownership checks.

## Timings and fencing

Defaults are leaseDurationSeconds=15, renewDeadlineSeconds=10 and
retryPeriodSeconds=2. All are positive integer seconds with lease > renew > retry;
the lease ceiling is 300 seconds. Non-integers, null, zero, negatives, wrong order,
over-ceiling values and a different leaderElectionLeaseName fail as configError
before any request. All replicas must use the same timings. A local monotonic
clock measures the renewDeadline from the beginning of a successful renewal
attempt; gate checks fail closed on that clock even if the renewal loop stalls.
A watchdog also cancels the tenure's duty context at that deadline.

Expiry is based on elapsed local time since the last observed Lease record
change, never on comparing remote renewTime with the local clock. With a healthy
API and prompt requests, crash takeover is between leaseDuration and
leaseDuration + retryPeriod from that observation: 15–17 seconds at defaults,
up to 19 seconds after the last renewal because observation may lag one retry.
API latency/unavailability can delay takeover; it never permits early takeover.
Graceful release permits acquisition on the standby's next retry.

Every process start has a fresh random identity, including restarts in the same
pod. Each acquisition increments spec.leaseTransitions; reporterEpoch equals
that value. Each tenure gets a fresh context and must start a fresh observationSeq.
Renewals retain the epoch. Acquisition, renewal and release use updates with the
freshly read resourceVersion. Conflicts trigger a read, never a forced write;
a foreign holder immediately closes the old gate. If a Lease is absent, creation
uses the highest epoch observed by that process plus one, or zero if none.
Epoch exhaustion fails closed.

Never delete the Lease manually. Deletion followed by a full process restart can
reset leaseTransitions, and Falcone refuses lower epochs: reporting becomes stale
but fails safe. Preserve the Lease and its epoch in recovery/restore. If history
has already been lost, restore a trusted epoch history or coordinate an audited
Falcone fencing-state recovery with its operator; do not bypass the epoch guard
or invent a replacement epoch in this controller.

## Leader loss

The standby does no pool writes, replica changes, deletes or publishes. A leader
that cannot renew closes its gate and cancels in-flight duties by renewDeadline,
before Lease expiry. It remains running as a standby and may acquire a higher
epoch later. No old gate can reopen. Crash handover obeys the observation-based
bounds above; existing worker pools continue serving with their existing state.

SIGTERM/SIGINT closes the gate, cancels duties and waits at most renewDeadline for
them to stop. Only then does the controller get and update its verified Lease to
clear holderIdentity, retaining the object and leaseTransitions. Each API request
is bounded by retryPeriod. If duties fail to stop, there is no release: the Lease
expires. A failed/conflicting release also leaves expiry to protect handover.
Old duties are never overlapped with new duties in the same process.

## API server unavailable

Errors/timeouts cannot authorize acquisition or extend the gate. After the local
renewDeadline there are zero pool writes, replica changes, deletes and publishes;
no pool mutation is attempted with stale access or ownership evidence. Before
that deadline, production duties are also no-ops. Lease attempts continue with
bounded requests and fixed error strings, while workers retain their last state.
The projected service account token is re-read for every Lease request. Responses
and tokens are never logged. Recovery re-establishes verified leadership first.

## Stale desired state

When Falcone desired state is unavailable or merely stale, there are zero pool
writes, replica changes and deletes. No deletion is inferred from missing data.
A future reconciler must require fresh authoritative desired state and verified
bindings before acting. Lease renewal continues while the API is available and
is the only Kubernetes write; stale state does not authorize a stale status
publication or a scaler action. The present no-op duties preserve this behavior.

## Failed preflight

A missing/invalid/mismatched POD_NAMESPACE or invalid timing produces one failed
pool-preflight/v1 report with configError, exit 1, no start, no access reviews and
no Lease traffic. Denied/error Lease access reviews also cause exit 1 and zero
Lease requests. There are zero pool writes, replica changes, deletes or publishes.
The pod is unavailable and restarts; no probe listener is introduced. Fix the
chart values, downward API or administrator-provisioned namespaced permissions.

## Rollout and review

The gntik-ai/falcone-charts deployment dependency is downward API POD_NAMESPACE
and restart-safe controller replicas. Chart implementation and deployment are
separate changes. R3 requires independent second review and operator approval.
Setting replicas=1 reduces contention but still requires election. Rollback is a
source revert; a retained Lease is inert. No runtime safety rule is relaxed.
