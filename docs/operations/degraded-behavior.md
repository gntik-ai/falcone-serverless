# Pool controller degraded behavior

The controller performs its read-only OPS-01 preflight, writes exactly one
`pool-preflight/v1` JSON report to stdout, and campaigns only on a passed result.
Runtime election diagnostics go to stderr on error-state transitions; repeated
standby or missing-history retries do not emit the same diagnostic indefinitely.
`POD_NAME` and `POD_NAMESPACE` must be
provided through the downward API. The namespace must equal the values file's
`controllerNamespace`, whose Lease permissions preflight checked. Pool namespaces
never select the election namespace. Leader election is enabled by default.

The only runtime Kubernetes write implemented today is maintenance of
`falcone-pool-controller-leader`. DEP-03 pool reconciliation and authenticated
pool-status delivery remain deferred. Those duties must use `Gate.Write` and its
cancellable context; every write must carry its epoch and sequence. Holding the
leader Lease does not authorize the separate scaler duty.
For pool-status delivery, `Gate.PublishReport` sets `reporterEpoch` and
`observationSeq` itself, overriding any caller-supplied values before invoking
the publisher. It consumes a sequence even when delivery fails.

## Four degraded modes

| Mode | Behavior |
| --- | --- |
| Leader loss | A standby performs no singleton duty writes. The previous leader closes its gate on observing a different holder, invalid ownership, missing continuity, cancellation, or expiration of its renewal deadline. Closing the gate cancels duty work. A healthy standby observing the last renewal takes over within that Lease's duration plus one retry period. |
| API server unreachable | A leader can use its last confirmed renewal only until the renewal deadline. An independent timer closes the gate even if an API request stalls. Afterward no reconcile, report, replica change or deletion is authorized. Lease API calls have a five-second request deadline. Recovery always starts with a fresh ownership-verified read; conflicts never trigger blind retries. |
| Desired state unavailable or stale | `Gate.Write(false, ...)` invokes no duty callback. No replica changes, deletion, or pool-status publication occurs from stale data. A Lease may still be renewed, but it grants no authority to act without fresh authoritative Falcone desired state and verified bindings. No desired-state cache or transport is introduced here. |
| Failed preflight | Configuration or permission failures emit the single failed preflight report and exit unsuccessfully. No election or Kubernetes object writes occur. Missing/invalid `POD_NAME`, missing `POD_NAMESPACE`, or a namespace mismatch fail before any API access. |

SIGTERM and SIGINT cancel the run context, immediately cancel duty contexts and
close the gate. Shutdown then reads the Lease afresh, verifies its complete owner,
UID, holder and epoch, and clears the holder using that read's resourceVersion.
It never deletes the Lease or discards the epoch. Shutdown API work is bounded to
five seconds. A Lease now held by another process is preserved.

## Epoch continuity and recovery

`falcone.ai/leader-epoch` records a nonnegative int64 high-water mark. Every new
term persists a strictly greater value with an optimistic-concurrency update
before opening its gate. Renewals keep the epoch. The in-memory sequence begins
at one for each new epoch and increases for every attempted duty write, including
failed deliveries. A process-specific holder nonce prevents a restarted pod from
continuing a former process's epoch and reusing its sequence numbers. Overflow,
malformed epochs and any observed epoch regression refuse publishing.

**There is no automatic epoch bootstrap or deleted-Lease recovery.** A missing
Lease has unknowable history: neither repeated 404s, a wall clock, nor the highest
epoch one surviving process remembers proves a global floor. The controller may
create an inert, owned Lease with no epoch, but it cannot open a gate on it. This
is deliberately fail closed, including on a first installation with no established
floor. No amount of waiting establishes epoch continuity.

Continuity must be provisioned by a trusted operator outside this controller.
`falcone.ai/leader-epoch-uid` must equal the API-assigned UID of the Lease, and
`falcone.ai/leader-epoch` must be at least every epoch ever persisted for this
singleton, including terms that did not publish. These annotations constitute a
trusted assertion of that floor. A verified first installation with no earlier
term can use floor zero, so the first actual term is one. If prior history cannot
be proven, leave the floor unset and publishing refused. Falcone's last accepted
report alone may undercount acquired but unpublished terms. Neither its receiver
nor a durable epoch authority is implemented by this ChangeSet.

Consequently, enabling election by default does **not** make a single replica
acquire automatically on a fresh install. The rollout requires a manual,
post-install continuity assertion after the Lease receives its UID. A Helm
template alone cannot supply that UID-bound assertion. Operator confirmation of
this bootstrap procedure and an operations / `gntik-ai/falcone-charts` follow-up
remain necessary; the controller must not weaken the continuity check to meet
the original automatic-acquisition rollout description.

Deletion loses the floor. Recreated/restored objects that copy old annotations
fail the UID binding check, even after process restart with no in-memory history.
An operator must establish a new valid floor and bind it to the replacement's
UID before campaigning resumes. Merely rebinding an old floor or restoring a
stale same-UID backup does not prove continuity and is unsupported. No algorithm
using only a deletable Lease can detect a privileged actor forging that assertion;
operator recovery must preserve the high-water mark. The controller never copies
or automatically manufactures this assertion after absence.

Every existing Lease must also have label `falcone.ai/owner=pool-controller`,
annotation `falcone.ai/ownership-version=1` and owner annotation
`falcone.ai/owner=["<controllerNamespace>","leader"]` (a JSON array of strings).
The duty identifier for this singleton is exactly `leader`; it is distinct from
the future scaler duty under the generic resource-ownership contract.
Missing, malformed or foreign ownership means no takeover, update or deletion.
Every update, including renewal and release, uses the resourceVersion and UID
from the same fresh read that passed these checks.
Lease timestamps use Kubernetes MicroTime's six fractional digits in UTC on
writes. Updates preserve `leaseTransitions`, `preferredHolder` and `strategy`
from the verified read; this controller does not manage those fields.

## Fencing and the residual overlap window

Receivers must implement the epoch guard and ordering in
[ctr-pool-status-report](../contracts/pool-status-report.md#ordering-cases): reject
an epoch below the stored epoch regardless of its generation or sequence, then
apply the generation/epoch/sequence ordering. After a newer epoch publishes, an
older epoch cannot overwrite it, even with a higher sequence or generation.
Before the newer epoch reaches the receiver, an old in-flight report can still
be accepted. Fencing protects only receivers and write paths that enforce it.
Later Deployment/Service writes need equivalent fencing or a reviewed proof of
idempotence; a local gate or Kubernetes resourceVersion alone does not supply it.

Gate timing is best effort. Defaults are lease/renew/retry = 15s/10s/2s. Accepted
lease durations are whole seconds from 3 through 60; renew is at least one second
and below lease; retry is at least 100ms and below renew. Takeover uses locally
observed unchanged Lease versions, not remote wall-clock timestamps. Slow clock
rates, process pauses, delayed or ambiguous API replies and lost history can
leave two processes believing they lead. **This overlap has no finite real-time
bound under arbitrary pauses or drift.** A delayed timer or cancellation cannot
retract an already in-flight write. Epoch fencing supplies correctness across
that window; the gate only reduces unnecessary work.

No maximum-duration no-history waiting barrier remains: waiting cannot recover
an epoch floor and cannot guarantee non-overlap. Any observation-based timing is
best effort, including with unequal accepted timing configurations (60/50/2 and
15/10/2 seconds). Tests deliberately allow overlapping gates in drift/deletion
scenarios and verify that the reference receiver rejects the superseded epoch.

Operational rollback remains an operator action: run one replica through chart
values or revert the delivery. A retained epoch annotation is harmless to the
placeholder controller, but deleting it sacrifices continuity for this election
implementation. R3 second review and operator approval remain delivery gates.
