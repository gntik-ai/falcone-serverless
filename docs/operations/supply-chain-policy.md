# Supply-chain policy gate

The offline Go test gate consumes **ctr-serverless-chart-values v1**. It guards
trusted platform image references, with no controller runtime import, network,
cluster access or Helm rendering. Run `go -C controller test ./...` to enforce it.
This R3 policy requires an independent second reviewer and operator approval.

## Rules and normalization

`image-digest-pinned` checks image objects with exactly `repository` and `digest`.
The digest must match `^sha256:[0-9a-f]{64}$`; a repository tag, embedded `@`,
extra key (including `tag`), missing key or wrong type fails. A required missing
image produces `<path>: missing` under this rule only.

`image-trusted-origin` normalizes bare `name` and `library/name` to
`docker.io/library/name`, `ns/name` to `docker.io/ns/name`, and `docker.io/name`
to `docker.io/library/name`. Explicit `docker.io/ns/name` stays unchanged.
Origin is independent of image pinning: tag/digest suffixes are stripped for
this check, so an otherwise trusted tagged image fails only the digest rule.
Malformed repositories fail closed. Every allowed prefix must end in `/` and
match the normalized repository at that path boundary: `docker.io/fs/` cannot
match `docker.io/fs-x/w`. An empty prefix list rejects every present entry.

`policy.trustedRepositoryPrefixes` comes from the deployment prefix file and
must be exactly `["docker.io/"]`, per the operator's Docker Hub only decision.
The evaluator also accepts namespace-scoped prefixes for boundary tests; the
deployment gate permits only the exact operator policy.

## Contract path table

`controller/internal/supplychain/testdata/chart-values-v1-images.json` has
`contractVersion: "v1"`, `ociEnablePath: "oci.enabled"`, and an `entries` array:

| path | component | kind | oci |
| --- | --- | --- | --- |
| `controller.image` | controller | image | false |
| `worker.image` | worker | image | false |
| `compiler.image` | compiler | image | false |
| `oci.intakeMirrorRepository` | intake | repository | true |

Each entry has these four fields. Validation rejects missing/unsupported
versions, empty enable paths or entries, duplicate/invalid paths, unknown kinds,
and absence of an image entry for controller, worker or compiler. Tests read the
schema and inventory by relative path, compare its exact-byte SHA256 with the
inventory pin, and resolve schema properties to check both path sets exactly.
Image objects are leaves: their internal repository member is not another entry.
Moving the Go package requires updating these relative paths and the doc path.

Non-OCI paths are required. OCI paths are required only when the enable path is
boolean `true`; absent and other values count as disabled. Every present OCI
entry is checked even when disabled. Contract v1 has no OCI image objects. Its
mirror is checked for origin only, and a required missing mirror produces
`oci.intakeMirrorRepository: missing` under the origin rule. Requiring this
mirror when enabled is deliberately stricter than the schema, which allows it
to be absent. This gate checks its repository location, not mirror contents or
tenant function images. OCI platform images require a later contract/table.

## Deployment input and refresh

`controller/internal/supplychain/testdata/deployment/` contains:

- `values.json`: synthetic contract-v1 values with `fs-test` identifiers and
  digest-pinned Docker Hub images. The synthetic digests do not identify built
  images. This proves the gate's wiring; it is not a real chart deployment.
- `trusted-prefixes.json`: exactly `["docker.io/"]`.
- `source.json`: either `{ "source": "synthetic", "contractVersion": "v1",
  "sha256": "<64 hex>" }`, with SHA256 matching the exact `values.json` bytes,
  or `{ "source": "falcone-charts", "repo": "<repository>", "commit":
  "<40 hex>", "path": "<source path>", "sha256": "<64 hex>" }`. The latter
  records the source file hash before JSON conversion, with non-empty repo/path.

Keep JSON files UTF-8 with LF endings and exactly one final newline. To refresh
synthetic values, keep them valid against the pinned schema and recompute the
exact-byte SHA256 into `source.json`. Replacing them with real chart values is
a follow-up once gntik-ai/falcone-charts ships the serverless chart: pin its
commit, record the original source file SHA256 and path, and review its JSON
conversion without fetching anything in tests. Never change the schema or
inventory as part of refreshing this gate. Report chart drift to falcone-charts.
Refreshes must pass `go -C controller vet ./...` and `go -C controller test ./...`;
`gofmt -l controller` must print nothing. Mutations of values, provenance and
prefix policy are exercised in memory; the checked-in files stay untouched.

`testdata/cases.json` is an array of `{name, values, policy, expected}` objects.
Each case supplies its own values and policy and a full expected report. It
covers the passing, pinning, origin, missing-path and OCI scenarios separately.
The gate is not full JSON Schema validation, signature/provenance verification,
vulnerability scanning, or authorization. Contract validation remains owned by
the published contract tests; signature enforcement is follow-up #119.

## Report and rollback

`Evaluate(values, table, policy)` returns JSON with `result: "pass"|"fail"`
and `rules` in fixed order: `image-digest-pinned`, then `image-trusted-origin`.
Each rule has `id`, `result`, and a lexicographically sorted `violations` array
of `<path>: <short reason>` strings. Passing rules use `[]`. Any violation makes
the report fail. Malformed values produce violations rather than a panic.
Reports never echo input values, credentials or Secret names, so no value needs
clipping (preflight's `clip()` otherwise limits echoed values to 40 runes).
Repeated calls produce byte-identical marshalled JSON and do not mutate inputs.

Rollout is merge-only. Reverting the package, fixtures and this document removes
the gate without changing runtime behavior. The committed synthetic copy can
drift from future chart defaults; review and refresh remain required.
