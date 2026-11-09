# Controller degraded behavior and leader election

Contracts: ctr-controller-config v1 (`docs/contracts/controller-config.md`),
ctr-resource-ownership v1 (`docs/contracts/resource-ownership.md`) and
ctr-pool-status-report v1, revision 1.2 (`docs/contracts/pool-status-report.md`).
This is OPS-01.t3, implementing FR-OPS-02. It describes what each controller replica
does when something it depends on is missing, and how an operator recovers.

Two rules hold in every degraded state below:

- **No replica changes.** A replica that cannot prove it leads, or cannot trust
  Falcone's desired state, changes no replica count.
- **No deletions.** It deletes no Deployment, Service or Lease for a pool.

## Leader election

Replicas elect one leader for the singleton duties through the Lease
`falcone-pool-controller-leader`. The election is in
`controller/internal/leader`.

- **Where.** The Lease lives only in the release namespace. Each replica reads its pod
  name and namespace from the downward API (`POD_NAME`, `POD_NAMESPACE`), and
  `POD_NAMESPACE` must equal `controllerNamespace` from the values file. There is no
  setting that moves the Lease to another namespace, and no Lease is ever created or
  read in a pool namespace. `POD_NAME` must be a DNS-1123 subdomain. A replica that
  fails either check reports the failure in the preflight report and exits 1 before it
  sends any request.
- **When.** A replica campaigns only after the preflight passed, so the Lease verbs in
  the release namespace are proven. A replica whose preflight failed never campaigns.
- **Timings.** `leaseDurationSeconds` 15, `renewDeadlineSeconds` 10 and
  `retryPeriodSeconds` 2 are the defaults, and they must satisfy
  `leaseDuration > renewDeadline > retryPeriod > 0`.
- **Takeover.** A standby takes the Lease when it is released, or when the holder has
  not renewed it for one `leaseDuration` as seen by the standby's own monotonic clock.
  A healthy standby therefore leads within `leaseDuration + retryPeriod` of the last
  renewal.
- **Step down.** The leader renews every `retryPeriod`. It closes its gate, cancels the
  duty work and stops publishing when a renewal conflicts, when the Lease is gone, or
  when it has had no successful renewal for `renewDeadline`.
- **SIGTERM.** The replica stops its duties first, waits for them for at most
  `renewDeadline`, and then releases the Lease (empty holder, epoch kept), so a standby
  takes over at once.

### Ownership and epoch checks

Before it takes over or updates an existing Lease, a replica verifies the whole
singleton ownership identity:

| Check | Required value |
| --- | --- |
| label `falcone.ai/owner` | `pool-controller` |
| annotation `falcone.ai/ownership-version` | `1` |
| annotation `falcone.ai/owner` | the JSON array `["<release namespace>","leader"]` |
| annotation `falcone.ai/leader-epoch` | a positive base-10 integer (the epoch of the latest term) |

A Lease that fails any check is never modified, and the replica reports
`OwnershipMismatch` (or `EpochInvalid` for the epoch annotation) without echoing any
value from the object. When no Lease exists, the controller creates it with that
metadata and epoch 1, so a single replica becomes leader on a fresh install.

### The fence

Correctness rests on fencing, not on timing. Each term has an epoch: the epoch recorded
on the Lease plus one, written with optimistic concurrency (`resourceVersion`). It never
comes from a wall clock, so it rises across handover and process restart. Every singleton
write and every pool-status report carries `reporterLineage` (the Lease `metadata.uid`),
`reporterEpoch` and `observationSeq`. The receiver records all three and refuses a
different lineage whatever its epoch, and a superseded epoch (see the contract). So a
write or report from a superseded epoch is never accepted after a newer epoch has
published.

## Degraded states

| State | What the controller does | Writes | Status reason |
| --- | --- | --- | --- |
| Leader lost (renewal conflict, Lease taken, renew deadline passed) | Closes the gate, cancels duty work, keeps campaigning as a standby | None from the old term. Its late reports carry the old epoch and are refused | `LeadershipLost`, then `Standby` |
| Healthy standby | Reads the Lease every `retryPeriod` and waits | None | `Standby` (ready) |
| API server unavailable or slow | A standby cannot read the Lease and stays unready. A leader keeps its gate only until `renewDeadline` after its last renewal, then steps down. An ambiguous Create or Update never makes a replica lead: only a successful answer does | No Lease write and no duty write unless the gate is open | `LeaseUnavailable` |
| Service-account token missing or unreadable | The token is reloaded on every Lease call. A call without a usable token fails closed and sends nothing | Same as API server unavailable | `LeaseUnavailable` |
| Falcone desired state stale or unavailable | The duties make no replica change and no deletion, and publish no report built from data they cannot trust. The Lease keeps being renewed, so the leader does not change | None for pools | Duty-level, outside the election |
| Failed preflight (verbs, values, `POD_NAME`, `POD_NAMESPACE`) | Prints the one `pool-preflight/v1` report, exits 1 and never campaigns | None | Report `failed` |
| Foreign or unverified Lease (label, version or owner annotation) | Never takes over or updates it | None | `OwnershipMismatch` |
| Missing or malformed epoch annotation | Fails closed and never modifies the Lease | None | `EpochInvalid` |
| Lease deleted while a replica leads | The renewal answers 404, so the gate closes at once. The replica creates a new Lease (epoch 1, new lineage), but the receiver refuses the new lineage, see below | Only the Lease create | `LeadershipLost`, then `FenceRejected` |
| Receiver refuses a report (`FencedError`) | Stops all duty writes, releases the Lease and stays stopped | None | `FenceRejected` |

A replica never infers the receiver's epoch or lineage from a refusal, never persists
one, and never retries with a higher epoch. `FenceRejected` stays until the process
restarts.

Readiness is reported through the status reason. Today a replica logs each change of
reason on stderr, one line per change, and never the token. Stdout carries only the
preflight report.

## The overlap window

Two replicas may both believe they lead for a short time. Nothing in the timings makes
that impossible:

- A leader paused for any time (a stopped process, a long GC, a slow API call) can
  resume and write once before it looks at its gate.
- A replica whose clock runs slow keeps its gate open longer. With a clock at half
  speed, a gate of `renewDeadline` lasts twice as long in real time, more than the
  `leaseDuration` a faster standby waits.
- A Create or Update whose reply is lost may have landed. Such a replica does not lead,
  and the standby takes over only after `leaseDuration` without a change.

In the usual case the old leader's gate closes at `renewDeadline` (10 s) after its last
successful renewal started, and a standby leads no earlier than `leaseDuration` (15 s)
after it last saw the Lease change, so the two do not overlap. The cases above are the
exceptions. The window lasts from the moment the new term has the Lease until the new
term's first report is accepted. During it, the receiver can still accept a report of
the old epoch, which was the legitimate leader's report when it was sent. From the first
accepted report of the new epoch on, every report of the old epoch is refused. Because
of the fence, the overlap does not change what the receiver ends up holding.

## Lease deletion and operator recovery

An operator who deletes the Lease starts a new lineage. Its epochs restart, so a new
term can reuse an epoch number the receiver already accepted from the old Lease, and
epoch numbers alone cannot tell the two apart. The receiver therefore refuses every
report of the new lineage, at any epoch, generation or sequence, and the controller ends
stopped with `FenceRejected`. This is an availability cost and it is intended. The
controller has no automatic recovery for a lost epoch or lineage.

To recover, an operator:

1. Confirms the cause: the Lease's `metadata.uid` differs from the lineage the receiver
   holds for this singleton.
2. Resets the receiver's recorded lineage and epoch for this singleton, so that it
   adopts the current Lease's lineage with the next accepted report. The reset is a
   Falcone-side operation.
3. Restarts the controller pods, because `FenceRejected` lasts for the life of the
   process. A fresh process reads the epoch from the Lease and publishes at epoch plus
   one.
4. Checks that the status is `Leading`, that its reports are accepted, and that a report
   delayed from the deleted lineage is still refused.

## Rollout, rollback and limits

- Leader election is on by default; a single replica still acquires the Lease.
- Kill switch: set the controller replica count to 1 in the chart values.
- Rollback: revert the commit. The strategy is merge-only, with no runtime adapter.
- Until Falcone's ingestion implements the lineage rule of revision 1.2, the fence has no
  effect at the receiver, so this behavior needs the follow-up issue in `gntik-ai/falcone`.
- The chart must set `POD_NAME` and `POD_NAMESPACE` from the downward API
  (`gntik-ai/falcone-charts`).
