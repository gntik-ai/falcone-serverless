# Revision runtime manifest

Contract: ctr-revision-runtime-manifest
manifestVersion: 1

This is the Wasm revision record contract for WAS-02.t2, implementing the
metadata boundary in design §3 and FR-DEP-03, FR-STO-04 and FR-WAS-03.
[revision-runtime-manifest.schema.json](revision-runtime-manifest.schema.json)
is the JSON Schema 2020-12 definition. `manifestVersion` is the string `1`.
This contract does not define OCI revision records (ADR 0010), runtime parsing,
grant approval, or a supported engine/language matrix.

## Immutable record

Falcone owns the authoritative revision and validates this record before storing
or dispatching it. Under FR-DEP-03, any change to code, artifact, language profile,
engine/WASI/host-interface versions, imports, budgets, grants, effective
configuration or provenance/SBOM references creates a new immutable revision.
Retain all pinned artifacts and references for the revision's retention period.
Never mutate a retained record in place or silently upgrade its engine/profile.

The worker's record/artifact consistency check belongs to WAS-02.t4: the worker
must refuse a mismatched record rather than infer or repair its fields. Validation
here does not implement that check or load/execute any artifact. Only validated
raw Wasm is input to trusted compilation; tenant-supplied precompiled artifacts
are never deserialized. See [artifact-validation-compile.md](artifact-validation-compile.md).

## Required fields

All of these top-level fields are required. Strings are non-empty unless a `const`
or digest pattern further constrains them. No additional top-level fields exist
in this version. Digests use `sha256:` followed by exactly 64 lowercase hex digits.

| Field | Meaning and shape |
| --- | --- |
| `manifestVersion` | Contract version, constant string `1`. |
| `artifactDigest` | Digest of the validated Wasm artifact, not a tenant native-code cache. |
| `languageProfile` | Closed object with required `name` and `version`; an exact approved profile pin, not an alias. |
| `engineVersion` | Exact qualified engine release selected by Falcone; the schema does not select a Wasmtime release. |
| `wasiVersion` | Exact WASI version supported by the selected profile. |
| `interfaceVersion` | Constant `0.1.0`, from the authoritative `falcone:host@0.1.0` package. |
| `imports` | Array of non-empty import names, recording the validated artifact's supported imports; empty means none. Falcone must verify compatibility and grant coverage. |
| `budgets` | Closed object: required positive integers `fuel` (guest computation units), `memoryBytes` (guest memory bytes), `timeoutMs` (invocation wall-clock milliseconds) and `maxHostCalls` (host-call count). These are limits, not observations. |
| `grants` | Array of verified capability grants as defined below; an empty array grants nothing. |
| `effectiveConfiguration` | The only open object. Keys follow both denylists below; values may be string, number, integer or boolean. Objects, arrays and null are rejected. Empty configuration is valid. |
| `provenance` | Closed object with required non-empty `ref` and SHA-256 `digest`, identifying immutable trusted-build provenance. |
| `sbom` | Closed object with required non-empty `ref` and SHA-256 `digest`, identifying an immutable SBOM. |

Falcone must verify digest/reference identity, reference authorization and
immutability, provenance trust, the supported version matrix, imports and
resource maxima; the schema validates their shape, not these semantic facts.
Example profile/engine names in fixtures are synthetic, not supported releases.

## Grants and scope derivation

Every grant is a closed object with required `family`, `interface` and `scope`.
It may additionally carry a `secretRef` or `guestGrantId` as described below.
The family selects both the exact interface and the closed scope shape:

| `family` | `interface` | Required scope field |
| --- | --- | --- |
| `storage` | `falcone:host/storage@0.1.0` | `prefixes`: allowed storage prefixes |
| `http` | `falcone:host/http@0.1.0` | `hosts`: allowed HTTP destination hosts |
| `event` | `falcone:host/events@0.1.0` | `topics`: allowed event topics |
| `workflow` | `falcone:host/workflows@0.1.0` | `targets`: allowed workflow targets |

Each scope field is a non-empty list of non-empty strings. A bare `*` is forbidden
in every list. A scope has exactly its family's field; cross-family fields and
unknown properties are rejected. Partial wildcard syntax is not interpreted by
this schema and confers no authority: Falcone must supply concrete, verified
scope entries under its grant policy. Interfaces are pinned to
[worker/wit/host.wit](../../worker/wit/host.wit), not arbitrary package names,
versions, WASI interfaces or ad-hoc host calls.

Falcone supplies verified scopes from authorized tenant/workspace bindings,
never guest fields. The host must enforce the verified invocation context,
operation authorization, deadlines and byte limits defined by
[ctr-wit-capability-host](../../worker/wit/README.md). HTTP additionally requires
host allow-list, DNS, redirect and rebinding validation; this record grants no
raw guest network, filesystem, environment or infrastructure access. Existing
restricted-pod and tenant-isolation requirements remain in force.

## Secrets and property-name rules

Secrets appear only as a `secretRef` reference or an opaque `guestGrantId`.
`secretRef` is a closed object with required non-empty `name` and optional
non-empty `version`. The name is an authorized binding identifier, never a
credential or direct infrastructure path. A reference with only `name` is valid;
Falcone owns binding/version resolution and rotation. `guestGrantId` is a
non-empty identifier for a separately reviewed narrow guest grant, never its
contents. These optional fields occur only on grants. The schema does not approve
a guest grant or authorize resolving a reference. Falcone and the host binding
layer must keep resolved secrets out of manifests, artifacts and logs. Secret
values stay in the host binding layer; only an explicitly reviewed narrow grant
may expose one to a guest. No implicit guest file or environment binding exists.

The global exact-name denylist rejects a property name whose lowercase spelling,
after removing every `_`, `-` and `.`, equals any of:

`value`, `plaintext`, `secret`, `secretvalue`, `password`, `passwd`, `token`,
`apikey`, `privatekey`, `credential`, `credentials`, `clientsecret`, `accesskey`.

It applies to every object, including `effectiveConfiguration`. For example,
`Password`, `API_KEY`, `client.secret`, `value` and `._-value-_.` are forbidden.
The schema declares no property whose name matches this denylist.

`effectiveConfiguration` additionally rejects a key if its normalized spelling
contains any of these substrings:

`password`, `passwd`, `secret`, `apikey`, `privatekey`, `credential`,
`accesstoken`, `authtoken`, `bearertoken`.

Thus `db-password`, `Bearer_Token` and `awsSecretKey` are forbidden;
`timeoutMs`, `maxTokens` and `logLevel` are allowed with scalar values.
`propertyNames` applies even though additional scalar configuration properties
are allowed. Every other object has `additionalProperties: false`.

## Executable checks and evaluator subset

`worker/tests/revision_runtime_manifest_contract.rs` runs under
`cargo test --workspace --locked`, without new crates. It evaluates every checked-in
JSON fixture under `fixtures/revision-runtime-manifest/valid/` and `invalid/`;
either directory being empty is an error. Fixtures contain only synthetic names,
digests and inert markers, with one defect per invalid manifest. It also checks
required-field omission coverage, all four families, the document version, WIT
interface pins, closed objects, declared names and denylist spelling variants.
Five in-memory schema mutations must each admit a named invalid fixture:
`unknown-top`, `missing-artifactDigest`, `configuration-db-password`,
`interface-unknown-package` and `cross-http-storage`.

The test evaluator supports exactly these schema keywords: `$schema`, `$id`,
`title`, `description`, `$defs`, `$ref`, `type`, `const`, `enum`, `required`,
`properties`, `additionalProperties`, `propertyNames`, `items`, `minItems`,
`minLength`, `minimum`, `pattern`, `not`, `allOf`, `oneOf`, `if` and `then`.
References are local `#/$defs/` references. Constants and enums are strings;
length/count limits are nonnegative integers. Unknown keywords or unsupported
regex syntax panic during schema inspection, including in unvisited branches.
Evaluator unit cases cover positive/negative semantics and unsupported constructs.

Patterns use an ECMA-262-compatible subset without flags: optional outer `^`/`$`
anchors, ASCII alphanumeric or colon literals, a single outer group with `|`
alternation, explicit case classes `[Xx]`, the separator class `[_.-]` optionally
followed by `*`, and the lowercase hex class `[0-9a-f]` optionally followed by
`{64}`. Empty branches, arbitrary ranges, nesting, escapes, lookarounds, other
quantifiers and inline flags are rejected. Matching is unanchored unless anchored
explicitly. Denylists use `[Xx]` for each letter and `[_.-]*` between letters;
global exact-name patterns also allow separators at both ends. Digest patterns
use `^sha256:[0-9a-f]{64}$`. This is standard 2020-12 schema syntax for Falcone's
Node/Ajv 2020 consumer, whose integration conformance remains a Falcone task.

## Limits, review and evolution

This schema checks shape only. It cannot detect a secret inside a valid string,
prove that scopes are authorized, verify provenance or enforce runtime isolation.
A denylist is incomplete and substring rules can reject legitimate keys. Falcone
must enforce verified scopes and keep resolved secrets out even for otherwise
valid manifests. Passing these tests is not runtime security qualification.

Changing these fields, scope shapes, pins, or either denylist requires a new manifestVersion;
never change a pinned version in place. Before Falcone pins this version, rollback
is a revert of this source change. After pinning, publish a new version and create
new revisions through Falcone. This R3 capability/egress/secret contract requires
an independent second reviewer and explicit operator approval before merge.
Grant-approval APIs (API-02), Falcone admission/storage pinning and its Node
consumer tests, runtime consistency (WAS-02.t4), charts and deployment are separate
tasks. The singular family labels `event`/`workflow` intentionally map to the WIT
interface names `events`/`workflows`; the operator should confirm that naming.
