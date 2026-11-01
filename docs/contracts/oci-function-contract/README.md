# ctr-oci-function-contract v1

Version: 1. Provisional legacy envelope baseline: `legacy-envelope-v1-provisional`.
Implements ADR 0010 D4 and OCI-02 ac2 only. HTTP cancel is deferred to OCI-04.
No worker runtime calls this tester. Later incompatible changes require a new version.

The trusted shim sends `POST /invoke` with `{params, context}` over a separate
localhost port. The execution credential stays in the trusted shim: **never send
credentials, tokens, secrets, tenant or workspace identity on this hop**. Context
contains only an opaque per-attempt `handle` (1–4096 Unicode characters, no
structure or identity semantics), `invocationId` (1–256 characters), and a
`deadline` (RFC 3339 UTC, `Z`, with optional 1–9 fractional digits). Handles expire
with the attempt. Reviewed capability grants belong to a separate contract.

## Normative rules

Each row is one stable rule ID and produces at most one tester diagnostic.
HTTP/1.1 uses Content-Length framing (no Transfer-Encoding), JSON encoded as UTF-8,
and `Connection: close`. Limits below count bytes, not Unicode characters.
Unknown application params are legal; the tester makes no assertion about their result.

| Rule ID | Requirement and observable check |
| --- | --- |
| OCI-V1-HEALTHZ | `GET /healthz` returns HTTP 200 within 2 s, after a startup allowance of 3 s. A missing or stalled health handler fails. |
| OCI-V1-INVOKE | `POST /invoke` returns HTTP 200. `GET /invoke` and `POST /__conformance_missing__` return 404 or 405. |
| OCI-V1-REQUEST | Accept exactly the request/context schemas; malformed JSON, missing/extra fields, invalid context and non-JSON Content-Type return HTTP 400. Valid requests built by the tester must validate. |
| OCI-V1-ENVELOPE | Every accepted invocation returns `application/json` and exactly `{status, result, logs}` matching envelope.schema.json (status membership is checked separately). `result` is any JSON value, `logs` a string. |
| OCI-V1-STATUS | Envelope `status` is `success` or `error`. Both use HTTP 200; errors encode their application result in `result`. These values are provisional legacy semantics. |
| OCI-V1-PARAMS-LIMIT | The compact UTF-8 JSON encoding of `params` is at most 6,000,000 bytes (6 MB decimal). Accept an exactly-at-limit value; reject a value one byte larger with HTTP 413 before invoking. |
| OCI-V1-RESULT-LIMIT | The compact UTF-8 JSON encoding of `result` is at most 6,000,000 bytes. The tester checks every returned result, including errors. |
| OCI-V1-LOGS-LIMIT | Decoded UTF-8 `logs` is at most 256,000 bytes (256 KB decimal), including any marker. |
| OCI-V1-LOGS-TRUNCATION | Truncate logs with the explicit suffix `[TRUNCATED]`, included in the budget. A full-budget log must end with this marker, even if it naturally fills the budget. Shorter logs may also carry the marker; whether they omitted original content cannot be inferred by the tester. |
| OCI-V1-PORT | `$PORT` is mandatory, a decimal port in 1–65535, with no default. The tester refuses to run either mode if it is missing or invalid. Process mode passes the validated port to the candidate. |
| OCI-V1-BINDING | Listen only on `127.0.0.1:$PORT`, never wildcard, another address or IPv6. Process mode inspects every TCP and TCP6 listener owned by the candidate process tree (the candidate, its process group, its descendants and orphans adopted by the tester), found through `/proc/<pid>/fd` socket inodes matched against `/proc/net/tcp{,6}`, while the candidate is running. Any listener other than `127.0.0.1:$PORT` fails, whatever its port; listeners of unrelated processes are ignored. Unavailable inspection fails closed. HTTP mode cannot establish process binding. |
| OCI-V1-SIGTERM | Exit after SIGTERM within 10 s. In-flight calls may finish within this grace. Process mode sends SIGTERM; after the grace it reports failure, kills the process group and reaps its child. |
| OCI-V1-FILESYSTEM | The runtime root is read-only; only `/tmp` is writable. The bounded tester supplies separate scratch TMPDIR and sentinel cwd/HOME. Any entry created, changed or removed in the sentinel is a violation; TMPDIR writes pass. |

The schemas express structural constraints. `x-maxSerializedBytes`,
`x-maxUtf8Bytes` and `x-truncationMarker` record the wire checks implemented by the
tester; generic JSON Schema validators do not enforce those annotations.

## Running the conformance tester

Build with `cargo build -p falcone-serverless-worker --bins --locked`.

- `PORT=18080 oci-function-conformance http` checks a running localhost candidate.
- `PORT=18080 oci-function-conformance process -- /absolute/candidate [args...]`
  starts a candidate with an empty environment except PORT, HOME and TMPDIR.
  Use an absolute executable path; no shell command is evaluated.

Use a free port. The tester never connects outside 127.0.0.1. It exits 0 on
conformance, 1 on rule violations (one `RULE-ID: diagnostic` line each), or 2
on CLI/infrastructure errors. A failure to start the candidate is infrastructure,
not evidence of contract conformance. HTTP I/O has an absolute 2 s budget per
request, bounded headers (16 KiB) and body (8 MB), with no unbounded read-to-EOF.
A candidate that fails health does not receive further HTTP probes.
Process cleanup always kills the tester-owned process group and waits for its
child, including early errors. Temporary directories are removed after inspection.

The filesystem sentinel covers cwd/HOME only, including nested files, symlinks
and metadata changes. It does not sandbox code or detect absolute-path writes
elsewhere. This tester is for operator-controlled local candidates, **never for
executing untrusted tenant binaries on a host**. Restricted pods enforce the full
filesystem and isolation boundary. No cluster access, credentials, image or shim
runtime change is part of this contract. HTTP mode omits process-only rules.

## Provisional legacy fixture pins

These are authored here under the operator decision of 2026-10-31; they are not
claimed to be MIG-01 qualification evidence. The source inventory remains
`docs/contracts/falcone-contract-inventory.md`; it supplies no envelope fixtures
at snapshot b253d026. MIG-01 must qualify, extend or replace this provisional
baseline, with an explicit contract revision if semantics differ.

| Fixture | SHA256 (exact file bytes, including final newline) |
| --- | --- |
| `fixtures/legacy/success.json` | `8e58a421ca36e8cf5fc9368e37b104ce4f0c1308770665724d52e6b855458da2` |
| `fixtures/legacy/error.json` | `0d3da0c8ec98ec48f9666ef37df5527b98b31ac713a8b2c8d98c6a61094e36ad` |
| `fixtures/legacy/null.json` | `651454783882a1f97a949d40d323f4ad23eb2875b0cc17be181f2b1a3eec83e6` |

## Embedded asset maintenance

`worker/src/oci_contract_assets/` is an exact source-local snapshot of this
contract directory. The unchanged worker Containerfile copies only `worker/src`,
so binaries embed these snapshots with `include_str!`/`include_bytes!` instead of
requiring docs in that build stage. When changing canonical docs or fixtures,
refresh the snapshot too. `source_only_image_context_has_identical_contract_assets`
and the fixture inventory test fail on any byte difference or missing fixture;
CI cannot accept a contract/code drift. The worker entry point and image's final
COPY remain unchanged; the conformance and reference tools are not shipped.
