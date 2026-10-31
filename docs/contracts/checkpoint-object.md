# Checkpoint object

Contract version: v1

`ctr-checkpoint-object` is the JSON metadata envelope for explicit application
checkpoints. It implements FR-JOB-02 and JOB-01 acceptance (2); see the design's
[JOB requirements](../falcone-serverless-design.md#job-long-running-analysis-and-recovery)
and [Long-running analysis](../falcone-serverless-design.md#long-running-analysis).
Later envelope versions must keep v1 readable for pinned revisions.

## Required fields

All fields are required. Unknown fields are rejected.

- `format_version`: unsigned 32-bit integer, fixed at `1` for this platform envelope.
- `job_id`: string identifying the job.
- `logical_invocation_key`: string identifying the logical invocation across retries (FR-WF-02).
- `attempt_id`: string identifying the producing attempt.
- `fencing_token`: unsigned 64-bit integer (`u64`) identifying the attempt fence.
- `sequence`: unsigned 64-bit integer (`u64`) ordering checkpoints within an attempt.
- `revision`: string identifying the pinned function revision.
- `profile`: string identifying the pinned runtime profile.
- `input_digest`: string identifying the job input digest.
- `checkpoint_schema`: string identifying the application-declared payload schema.
- `committed`: boolean indicating whether the checkpoint is committed and eligible for resume.
- `payload_ref`: string referencing the checkpoint payload in host-managed storage.

`format_version` versions the platform envelope; `checkpoint_schema` identifies
the application's format. These are separate compatibility checks. Consumers
must preserve `u64` integers exactly: JavaScript JSON numbers lose precision above
2^53. Falcone consumers must use lossless integer parsing rather than round these
tokens through JavaScript `Number`.

## Rules

A commit whose `fencing_token` differs from the current attempt fence is rejected (stale-fence rejection).
Only an equal token is accepted: a lower token returns `StaleFence`, while a higher
token returns the distinct `FutureFence` error. Fence issuance belongs to
STO-03/JOB-02. The pure commit check does not write a payload or mark it committed.

Resume accepts only committed checkpoints whose `checkpoint_schema`, `input_digest`, `revision` and `profile` match the job.
Unsupported envelope versions return `UnsupportedFormatVersion`. A v1 resume
check returns `NotCommitted` before compatibility checks; compatibility errors
are `SchemaMismatch`, `InputDigestMismatch`, `RevisionMismatch` and
`ProfileMismatch`, checked in that order. Identity matching for `job_id` and
`logical_invocation_key`, and selecting the latest checkpoint, belong to JOB-02.

Guest memory is never transparently restored.
The application explicitly decodes its saved state and continues computation.

The payload is opaque application bytes with no content type.
The application interprets those bytes according to `checkpoint_schema`.
Storage binding and WIT capability implementation belong to JOB-01.t5; retention,
garbage collection and payload checksums belong to STO-01.

## Canonical example

```json
{
  "format_version": 1,
  "job_id": "job-analysis-42",
  "logical_invocation_key": "analysis-42",
  "attempt_id": "attempt-3",
  "fencing_token": 7,
  "sequence": 2,
  "revision": "revision-abc123",
  "profile": "analysis-v1",
  "input_digest": "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
  "checkpoint_schema": "analysis-state/v1",
  "committed": true,
  "payload_ref": "seaweedfs://checkpoints/job-analysis-42/attempt-3/2"
}
```
