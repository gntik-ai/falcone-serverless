# Controller leader election and degraded behavior

Contracts: ctr-controller-config v1 (`docs/contracts/controller-config.md`),
ctr-resource-ownership (`docs/contracts/resource-ownership.md`) and
ctr-pool-status (`docs/contracts/pool-status-report.md`). This is OPS-01 and
FR-OPS-02. Code: `controller/internal/leader` and `controller/main.go`.

## Leader election

The controller runs as several replicas. One of them leads. Only the leader runs
the singleton duties (reconcile, scaler, status publication) and only the leader
publishes the ctr-pool-status report. The election uses one Lease,
`falcone-pool-controller-leader`, in the controller's own release namespace. The
namespace comes from `POD_NAMESPACE` (downward API) and must equal
`controllerNamespace` in the values file. No configuration selects another
namespace or name, and no election request ever reaches a pool namespace.

| Item | Value |
| --- | --- |
| Lease | `coordination.k8s.io/v1` `falcone-pool-controller-leader` in `POD_NAMESPACE` |
| Ownership metadata | label `falcone.ai/owner=pool-controller`, annotation `falcone.ai/ownership-version=1`, owner annotation `["<namespace>","falcone-pool-controller-leader"]` |
| Timings | `leaseDuration` 15s, `renewDeadline` 10s, `retryPeriod` 2s (contract defaults; the values file keys are not read yet) |
| Validation | all positive, lease > renew > retry, a whole number of seconds, lease at most 5 minutes; anything else fails closed |
| Holder identity | `POD_NAME` plus a random per-process suffix, so a restarted container never reuses an earlier hold |
| Fencing token | `leaseTransitions`; it is the `reporterEpoch` of every published report |

An existing Lease without the label and annotation above is never taken over; the
controller stays a non-leader. A Lease the controller creates carries them.

### Timing rules

- Expiry is decided only from the controller's own monotonic clock. The Lease's
  `renewTime` and `acquireTime` are informational and are never used to decide, so
  clock skew between replicas cannot open a gate.
- A candidate takes over a record it has watched stay unchanged for the greater of
  its own lease duration and the record's `leaseDurationSeconds`. It acquires no
  earlier than that, and within two retry periods after it.
- The leader's gate is open until the start of its last successful renewal request
  plus `renewDeadline`. The gate is evaluated at every check, so a hung request
  cannot extend it.
- A write with a stale `resourceVersion` gets 409 and means "not acquired". Release
  uses an update, never a delete, so `leaseTransitions` survives.
- Only HTTP 404 means "not found". 401, 403, 5xx, redirects, timeouts and malformed
  or oversized bodies are API failures and open nothing.

### Recovery barrier

A 404 hides the previous holder's last renewal, however closely the Lease was
polled: reading an unchanged record cannot show that nobody renewed between the
last read and the deletion, and that renewal keeps the holder's gate open for up to
`renewDeadline`. So every 404 fails closed, including one right after a successful
read. It starts a barrier at its first successful API response and waits one full
lease duration, on its own clock, before it creates the Lease. An API error during
the barrier restarts it from the next success. A Lease that reappears cancels it and
observation applies. With the default timings:

- B observes A's Lease at t=0, then loses API access while A renews through t=14.
- The Lease is deleted at t=15 and B sees 404 at t=16. A, partitioned, keeps a valid
  gate until t=24.
- B's barrier starts at t=16, so B sends no create and opens no gate before t=31.

The same holds when B polls without a gap: B reads A's unchanged record at t=0 to
t=8, A renews right after the read at t=8 (earlier renewals failed), the Lease is
deleted at t=9 and A is partitioned, so A's gate stays open until t=18. B sees 404 at
t=10, starts the barrier there and creates nothing before t=25.

The first start on an empty namespace therefore waits one lease duration before the
first acquisition: a first install cannot be told apart from a deleted Lease. When
the barrier ends in a create, `leaseTransitions` continues from the highest value
this process has seen, plus one. A process with no history at all starts at 0; the
fencing counter is then not monotonic, and Falcone refuses lower-epoch reports
(safe, but status stops until the epoch passes the stored one).

## Shutdown

On SIGTERM or SIGINT the leader closes its gate first, cancels the duties, waits for
them to drain for at most `leaseDuration - renewDeadline`, then releases the Lease by
an update (empty holder) with a `resourceVersion` precondition. A standby acquires
the released Lease on its next retry. A non-holder, or a drain that did not finish,
releases nothing, and the Lease expires normally. The process then exits 0.

## Requirements on duty writers (DEP-03)

- Run only behind the gate: check it before every write and stop when the duty's
  context ends. `leader.Admission` does the check; `leader.GuardedWriter` applies it
  to every write and delete.
- In-flight work must finish within `leaseDuration - renewDeadline` (5s by default)
  after the context ends. A successor cannot hold the Lease earlier than that.
- Desired state older than 60s (`leader.DefaultMaxDesiredStateAge`) is stale.
- Publish only through `leader.Reporter`, which takes `reporterEpoch` from the
  leadership token and restarts `observationSeq` when the epoch rises.

## Degraded states

In every state below the controller changes no replica count on stale data and
deletes nothing. Existing worker Deployments and Services keep running; nothing is
removed to repair a degraded controller.

### Leader loss

The replica that lost the Lease (404 or 409 on renew, another holder, or no
successful renewal within `renewDeadline`) closes its gate at once, cancels its duties
and returns to candidate. It never recreates the Lease while its gate is open. It
writes and publishes nothing from then on. Another replica takes over after the
timing rules above. With no standby, no one writes until the controller returns.

### API server unavailable

Renewals fail, so the gate closes `renewDeadline` after the last successful renewal
started, and the leader then writes and publishes nothing. A replica that cannot
reach the API sends no election write and, if it had no history, starts the recovery
barrier when the API returns. Writes made inside the `renewDeadline` window are
those of a still-valid leader.

### Stale or unavailable desired state

When Falcone's desired state is unavailable or older than the staleness limit, the
admission check refuses every write and delete, even for an open gate. Replica counts
stay as they are. The leader keeps its Lease, so a recovery needs no election.

### Failed preflight

When the OPS-01 preflight is `failed` (a denied or errored access check, a missing
prerequisite or a configuration error), the process prints the report and exits 1.
It sends no Lease request and starts no duty. The same applies when `POD_NAMESPACE`
or `POD_NAME` is absent or invalid, or `POD_NAMESPACE` differs from
`controllerNamespace`: exit 1 with the preflight report intact. A restarting pod is the unready
state, because no probe listener exists yet.

## Rollout and rollback

The chart must render `POD_NAMESPACE` and `POD_NAME` from the downward API before
this image runs; until it does, the pod exits 1. Replica count 1 is the kill
switch; it still acquires the Lease. Rolling back is a revert; a leftover Lease is
inert and needs no migration.
