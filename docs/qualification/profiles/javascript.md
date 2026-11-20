# JavaScript Wasm profile qualification

Profile `javascript-wasi-v1`, matrix revision `2`
([matrix](../../contracts/language-profile-matrix.md), WAS-03, FR-WAS-05).

**Status: `disabled`, not qualified.** Nothing in this document enables the profile. It
becomes qualified only when every static and cluster check passes; any failure records it as
`blocked`. Built components and their evidence do not exist yet: every runtime result below
is `UNVERIFIED` until an operator runs the trusted-image build and the cluster suite and
records the artifact ID and sha256 on the story issue (gntik-ai/falcone-serverless#53).

The manifest is [worker/qualification/profiles/javascript/manifest.toml](../../../worker/qualification/profiles/javascript/manifest.toml).
A static test keeps this page, the manifest and both matrix copies consistent.

## Versions

| Component | Version |
| --- | --- |
| Engine | StarlingMonkey (SpiderMonkey), prebuilt inside ComponentizeJS |
| Wasmtime | `49.0.5` |
| `@bytecodealliance/jco` | `1.41.0` |
| `@bytecodealliance/componentize-js` | `0.23.0` |
| `wasi-virt` | git revision `bca61c27499d2c290b5431cfcff013737b3ba01d`, built with `--locked` in the trusted image |
| Node.js (runs jco in the trusted image) | `v26.13.0` |
| WASI | `none` |
| WIT package | `falcone:host@0.2.0`, world `function` |
| Package catalogue | unqualified and empty: no third-party package is accepted |

The pins are copied exactly from `worker/qualification/javascript/versions.toml`, which holds
the sha256 of each archive. Fixtures are built only inside the trusted compiler image, with no
network, and a sha256 mismatch fails the build closed.

## Interfaces

The final component may import only these `falcone:host` interfaces (a ceiling, not a grant;
verified invocation grants still apply):

- `falcone:host/storage@0.2.0`
- `falcone:host/http@0.2.0`
- `falcone:host/events@0.2.0`
- `falcone:host/workflows@0.2.0`
- `falcone:host/logging@0.2.0`
- `falcone:host/cancellation@0.2.0`
- `falcone:host/progress@0.2.0`
- `falcone:host/invocation-context@0.2.0`

No WASI interface is allowed. `validate.rs` stays the import gate after the build and rejects
every other import, WASI included. A precompiled Wasmtime artifact is never deserialized, and
no fixture is or contains one (tested).

## Tested examples

| Example | What it shows | Imports |
| --- | --- | --- |
| `json-transform` | A pure JSON transformation. | none |
| `text-regex` | Regular expressions over text. | none |
| `hash-sha256` | SHA-256 through Web Crypto `crypto.subtle.digest`, with no polyfill. | none |
| `events-handle` | A real `handler.handle` that reads and updates a rate-limit bucket in storage and publishes an access event. | storage, events |

The recorded inputs and expected outputs are under `workloads/` and `examples/`. The
expected outputs of the three pure workloads are locally created worked examples (the
`hash-sha256` output is the FIPS 180-2 test vector for `abc`), checked independently in the
static suite. They are **not** the outputs recorded for #138, which this snapshot did not
contain, so they stay pending: the manifest marks each `expectation = "pending-138"`, a match
is reported UNVERIFIED and never counts towards qualification, and a mismatch fails. Only
replacing them with the #138 recording (`expectation = "recorded"`) lets them qualify.

`events-handle` runs with narrowly scoped grants, present in both the credential and the
projection: the event topic `console.openapi.spec.accessed` and the single storage
key `tenants/synthetic/rate/ws-synthetic-0001` it uses (another workspace's key is denied), with
no wildcard and no credential value. Its recorded dependency state
(the stored bucket) is replayed before each invocation. The comparison is exact: storage keys,
checkpoint flags, event destinations and payload bytes, in order, plus the response. A
changed key, destination, payload or response, or a dropped replay, fails the comparison and
the computed status stays not qualified.

## Supported and unsupported behaviour

Supported for qualification: pure computation, `TextEncoder`/`TextDecoder`, JSON, regular
expressions, Web Crypto (`crypto.getRandomValues`, `crypto.subtle.digest`) and the host
interfaces above. Anything else is rejected before any build or instantiation.

| Code | Meaning |
| --- | --- |
| `WASM-NATIVE-LIBRARY` | A native library or addon (`.node`, `process.dlopen`). |
| `WASM-GPU` | GPU access. |
| `WASM-PROCESS-SPAWN` | `child_process`, `worker_threads` or `cluster`. |
| `WASM-INSTALL-HOOK` | An install or lifecycle script. |
| `WASM-PACKAGE-NOT-IN-CATALOGUE` | A dependency or version outside the pinned catalogue. |
| `WASM-UNSUPPORTED-API` | Anything else unsupported: `node:*` modules, `node:crypto`, `process.env`, `eval`, `new Function`, non-literal `require`/`import`, an unresolved or tree-escaping relative import. |

Every diagnostic names the construct and its file and line, never tenant data. A message may
name OCI mode as an alternative, but no diagnostic selects or changes the execution mode; the
mode stays `wasm` (tested). Each code has a negative fixture under `negative/`.

The tree check (`compat::check_tree`) is separate from the legacy adapter's single-source check.
It follows relative imports inside the supplied tree and never accepts a multi-file source for
the adapter's build path.

## Reference function: BLOCKED migration result

The reference function is `packages/openapi-sdk-service/actions/openapi-spec-serve.mjs` in
gntik-ai/falcone at `10a3c6e7a23c9a29b934c35f0682bbdef232099f` (operator decision 2026-11-20).
It cannot run under this profile, and no shim, polyfill or wrapper is added. The static check
rejects the unmodified source tree; nothing is componentized or executed. The result is
`blocked`. The function must move to OCI mode (separately authorized) or be rewritten.

Diagnostics, each with file and line:

- `WASM-UNSUPPORTED-API`, `node:crypto` (`createHash`): `src/spec-cache.mjs` line 1.
- `WASM-UNSUPPORTED-API`, `process.env` reads: `src/config.mjs` lines 13, 39, 40, 41, 42, 43, 44, 45, 47 and 50.
- `WASM-PACKAGE-NOT-IN-CATALOGUE`: `pg` (`actions/openapi-spec-serve.mjs` line 1) and `kafkajs` (line 2).

The closed grammar can add further `WASM-UNSUPPORTED-API` diagnostics for files with no
feature hit; they are recorded, never filtered.

Verbatim copies are committed as test fixtures only under
`worker/qualification/profiles/javascript/legacy/`, with these sha256 values:

| File | sha256 |
| --- | --- |
| `actions/openapi-spec-serve.mjs` | `3827f5e2c7c14cc3cf5abb4eb8272d8cb67938dc4d195723efcf695bf8924ce1` |
| `src/config.mjs` | `749d3fefde37945e57607ae462a6a55ee193181c898ae4bbbd641f1dae0a30e5` |
| `src/spec-version-repo.mjs` | `5b13e9bd885954900ddf1d4e2d7b780d5e46d73e09074b31bdf5aa013f5de437` |
| `src/spec-cache.mjs` | `74883a947b993384a4fe3fd697e7cf4f73bdb2f36ac688ec4303bea43d17e2dc` |
| `src/spec-audit.mjs` | `28dcd89088ee98857dff89cca7aab62bc4d37fea2996fb31252cf4fc83089fd6` |
| `src/network.mjs` | `7b8f7a0b7d788b1b5b4b7c16a62a74d8900f521facaa8e94c9c30ea9a85eb270` |

The default test verifies the hash of every checked-in file and fails on a mismatch. A file
that is absent is reported `UNVERIFIED` and skipped; it never passes as qualified.

`UNVERIFIED` today: `src/spec-audit.mjs` and `src/network.mjs` are not in the maker snapshot
(it has no network), so the diagnostics of the complete six-file tree are not recorded.
`expected-diagnostics.json` is `pending` with the operator-specified diagnostics as its
required list, and `expected-diagnostics-partial.json` records the four-file result for
regression only. To complete it, an operator fetches the two files by commit, checks the
hashes, reruns `profile_javascript` and records the computed diagnostics.

### Access event

For a future rewrite: the function emits topic `console.openapi.spec.accessed` with payload
`{eventType: "openapi.spec.accessed", timestamp, workspaceId, tenantId, specVersion, requesterId, format}`,
fire-and-forget (`void emitSpecAccessed(...).catch(() => undefined)` in `src/spec-audit.mjs`).
The wrapper does not run, and the legacy envelope fixtures of #278 are unchanged.

## Evidence

| Check | Where | Result |
| --- | --- | --- |
| Matrix, manifest, interfaces, docs, negative fixtures, mutation tests | `cargo test --workspace --locked` (`profile_javascript`, `language_profile_matrix`, `compat_tree`) | Static; runs in CI. |
| Reference tree is blocked | `profile_javascript` | Four files verified; two `UNVERIFIED`. |
| Build of the four positive components | `worker/qualification/javascript/build-profile.sh`, operator-run in the trusted image | `UNVERIFIED`: not run. |
| Positive workloads and the `events-handle` example in a restricted pod | `cluster_profile_javascript` (`#[ignore]`, `FS_TEST_CLUSTER`, digest-pinned `FS_TEST_WAS02_IMAGE`) | `UNVERIFIED`: not run. |

The cluster suite creates a fresh `fs-test-*` namespace, runs a restricted pod (Pod Security
restricted, seccomp `RuntimeDefault`, read-only root filesystem, arbitrary non-root UID),
deletes the namespace and touches no other. No artifact ID or sha256 is recorded yet.

FR-WAS-06 fuzz, leakage and crash-recovery evidence is out of scope, so the matrix evidence
stays empty and the profile stays `disabled`.
