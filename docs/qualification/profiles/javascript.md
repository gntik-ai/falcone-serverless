# JavaScript Wasm profile qualification

Profile `javascript-wasi-v1`; runtime mode `wasm`; computed verdict **blocked**.
The matrix revision is `2` and its JavaScript status remains `disabled`.
Recording toolchain pins or this report does not enable a profile. R3 second review
and operator approval remain required before merge. No cluster run, component build
or runtime qualification is claimed by this source-only evidence.

## Exact pins

The 2026-11-19 operator decision is recorded in
[versions.toml](../../../worker/qualification/javascript/versions.toml).
The engine is StarlingMonkey, distributed inside the pinned ComponentizeJS
archive; it has no independently selected engine release. Node runs only in the
trusted compiler image. The package catalogue is `unqualified` and empty: no
third-party package or local-module dependency is supported by the current checker.

| Component | Version | Source | SHA256 or git revision |
| --- | --- | --- | --- |
| @bytecodealliance/jco | `1.41.0` | https://registry.npmjs.org/@bytecodealliance/jco/-/jco-1.41.0.tgz | abe7ab13d0fb1b6ecc6f7d3fffd6ff27862d158936a11d6f48bcb0ada00af4c6 |
| @bytecodealliance/componentize-js | `0.23.0` | https://registry.npmjs.org/@bytecodealliance/componentize-js/-/componentize-js-0.23.0.tgz | b2e4b6fdee055805370950f127c21d7c549dcc7a1e907f20c5ee769ebba7ad10 |
| wasi-virt | `0.2.0` | https://github.com/bytecodealliance/WASI-Virt | bca61c27499d2c290b5431cfcff013737b3ba01d |
| Node.js | `v26.13.0` | https://nodejs.org/dist/v26.13.0/node-v26.13.0-linux-x64.tar.xz | d28a96af6c38596f6d9d97c0ee531c13d238d757742387d1928de822d959e997 |
| wasmtime | `49.0.5` | Workspace / authoritative WIT | No separate artifact |
| falcone:host | `0.2.0` | Workspace / authoritative WIT | No separate artifact |
| wasi | `none` | No WASI imports | No separate artifact |

No toolchain is downloaded, upgraded or built here. The git-pinned wasi-virt
is the WASI-Virt project, not the unrelated crates.io package.

## Import boundary

Only the eight imports parsed from `worker/wit/host.wit` are permitted:

- `falcone:host/storage@0.2.0`
- `falcone:host/http@0.2.0`
- `falcone:host/events@0.2.0`
- `falcone:host/workflows@0.2.0`
- `falcone:host/logging@0.2.0`
- `falcone:host/cancellation@0.2.0`
- `falcone:host/progress@0.2.0`
- `falcone:host/invocation-context@0.2.0`

The guest exports `falcone:host/handler@0.2.0`; that export is not a grantable
import. WASI is `none`. The existing validator remains the final import gate,
including UTF-8 canonical-string requirements. There is no built reference
component, so its actual import check is blocked rather than asserted to pass.

## Reference and examples

Public source: `gntik-ai/falcone` revision
`10a3c6e7a23c9a29b934c35f0682bbdef232099f`, originally under
`packages/openapi-sdk-service/`. The following files are committed verbatim:

| File relative to reference/ | SHA256 |
| --- | --- |
| actions/openapi-spec-serve.mjs | 3827f5e2c7c14cc3cf5abb4eb8272d8cb67938dc4d195723efcf695bf8924ce1 |
| src/config.mjs | 749d3fefde37945e57607ae462a6a55ee193181c898ae4bbbd641f1dae0a30e5 |
| src/spec-version-repo.mjs | 5b13e9bd885954900ddf1d4e2d7b780d5e46d73e09074b31bdf5aa013f5de437 |
| src/spec-cache.mjs | 74883a947b993384a4fe3fd697e7cf4f73bdb2f36ac688ec4303bea43d17e2dc |

The transitive closure is incomplete: `src/network.mjs` (imported by config)
and `src/spec-audit.mjs` (imported by the handler) lack pinned public sources
and hashes in the supplied decision. Neither is fabricated or replaced.

All eight [legacy-envelope fixtures](../../contracts/fixtures/legacy-envelope/valid/) are mapped
without editing their sources or expectations. These are recorded examples, not
successful JavaScript execution evidence:

| Envelope fixture | Expected dependency calls |
| --- | --- |
| [401](../../contracts/fixtures/legacy-envelope/valid/401.json) | None |
| [400](../../contracts/fixtures/legacy-envelope/valid/400.json) | None |
| [429](../../contracts/fixtures/legacy-envelope/valid/429.json) | None |
| [404](../../contracts/fixtures/legacy-envelope/valid/404.json) | storage.read |
| [403](../../contracts/fixtures/legacy-envelope/valid/403.json) | storage.read |
| [304](../../contracts/fixtures/legacy-envelope/valid/304.json) | storage.read |
| [200-json](../../contracts/fixtures/legacy-envelope/valid/200-json.json) | storage.read, events.publish |
| [200-yaml](../../contracts/fixtures/legacy-envelope/valid/200-yaml.json) | storage.read, events.publish |

The fixtures contain synthetic params, expected statusCode/headers/body and
dependency state, including rate-limit history. The current single-source
closed grammar and empty catalogue cannot support this handler. Mapping pg
Pool.query, Kafka publish and rate-limit dependency injection onto the host
requires separate operator decisions and adapter work. This change adds no shim.

## Diagnostics

Every rejection precedes componentization or instantiation. Diagnostics include
only a construct and source/line/column, never tenant values. Source indices are
zero-based, line/column are one-based. Package manifests use source index 1 in
the single-source negative fixtures.

| Synthetic fixture | Code | Construct | Source:line:column |
| --- | --- | --- | --- |
| negative/native-addon.json | `WASM-NATIVE-LIBRARY` | .node | 0:1:9 |
| negative/process-spawn.json | `WASM-PROCESS-SPAWN` | child_process | 0:1:9 |
| negative/install-hook.json | `WASM-INSTALL-HOOK` | lifecycle script | 1:1:2 |
| negative/gpu.json | `WASM-GPU` | navigator.gpu | 0:1:1 |
| negative/package.json | `WASM-PACKAGE-NOT-IN-CATALOGUE` | package import | 0:1:9 |
| negative/unsupported-api.json | `WASM-UNSUPPORTED-API` | node:crypto API | 0:1:9 |

The static suite records the actual `compat::check` results:

| Input | Code | Construct | Line:column |
| --- | --- | --- |
| four-source bundle | `WASM-UNSUPPORTED-API` | source bundle | 1:1 |
| actions/openapi-spec-serve.mjs | `WASM-PACKAGE-NOT-IN-CATALOGUE` | package import | 1:16 |
| actions/openapi-spec-serve.mjs | `WASM-PACKAGE-NOT-IN-CATALOGUE` | package import | 2:23 |
| actions/openapi-spec-serve.mjs | `WASM-PACKAGE-NOT-IN-CATALOGUE` | package import | 3:24 |
| actions/openapi-spec-serve.mjs | `WASM-PACKAGE-NOT-IN-CATALOGUE` | package import | 4:32 |
| actions/openapi-spec-serve.mjs | `WASM-PACKAGE-NOT-IN-CATALOGUE` | package import | 5:43 |
| actions/openapi-spec-serve.mjs | `WASM-PACKAGE-NOT-IN-CATALOGUE` | package import | 6:34 |
| src/config.mjs | `WASM-PACKAGE-NOT-IN-CATALOGUE` | package import | 1:41 |
| src/spec-cache.mjs | `WASM-UNSUPPORTED-API` | unresolved syntax | 8:12 |
| src/spec-version-repo.mjs | `WASM-UNSUPPORTED-API` | unresolved syntax | 1:23 |

The bundle reports `WASM-UNSUPPORTED-API` / source bundle at source 0, 1:1.
Per-file checks additionally expose package import and unresolved syntax
diagnostics. In particular the public spec-cache source is not rewritten to
hide node:crypto createHash; its exact source currently reports unresolved syntax.
The separate negative API fixture records node:crypto API rejection.
The diagnostic list is not an exhaustive inventory of all unsupported constructs:
the closed checker can stop at the first syntax or bundle limitation.

OCI may be mentioned as a separately authorized alternative in diagnostic text.
No diagnostic, doc or computed verdict changes runtime_mode from wasm. Tenant
native binaries and precompiled Wasmtime artifacts are absent and never loaded.

## Verdict and evidence

Qualification requires every static check and passing cluster evidence for all
eight envelopes. Missing evidence or any failed check yields blocked. The tests
force each individual static check and envelope comparison to fail, and check
that a source hash mismatch also blocks qualification. Matrix status remains disabled.

| Evidence | Artifact ID | SHA256 |
| --- | --- | --- |
| Source-only checks and exact diagnostics | `repo:worker/qualification/javascript/evidence/static.json` | `c4332a154081257dd0e50a869fd1b26afc5aedc3932a478aeeb973dc7bca951c` |
| Built reference imports | UNVERIFIED: no artifact | None |
| Eight cluster envelopes and dependency calls | UNVERIFIED: no artifact | None |

The static JSON artifact is committed alongside the manifest; the static test
recomputes its checks, diagnostics and verdict and checks its receipt. The false
checks are compatibility, sourceClosure and componentImports. No fake artifact
ID or digest is assigned to unavailable build or cluster evidence.

## Operator-run build and cluster suite

Default `cargo test --workspace --locked` performs source-only qualification.
It needs neither FS_TOOLCHAIN_CACHE nor a cluster. The ignored build and runtime
checks remain UNVERIFIED. All guest execution lives in cluster_profile_javascript.rs.

Building is available only through `worker/qualification/javascript/build-fixtures.sh --profile`
inside the offline trusted compiler image, with FS_TRUSTED_COMPILER_IMAGE=1,
FS_TOOLCHAIN_CACHE, FS_LEGACY_COMPONENTS (a new output directory) and TMPDIR.
The script first runs the source eligibility gate. It currently refuses this
reference before invoking jco, wasi-virt or any guest. After future reviewed
adapter work permits it, the script verifies the exact cached archives and
wasi-virt revision, componentizes against world function with all optional WASI
disabled, applies deny-all wasi-virt, and validates final imports. It emits a raw
reference.wasm plus SHA256SUMS and the prebuilt qualification runner. An operator
must then record its real artifact ID, SHA256 and relative path in the manifest.

The digest-pinned qualification image must contain the source checkout under
`/opt/falcone`, including this manifest, WIT, inventory fixtures and raw component,
plus `/usr/local/bin/profile-javascript` and `artifact-compile-child` from that build.
With static checks passing, an operator runs:

```sh
FS_TEST_CLUSTER=1 FS_TEST_WAS02_IMAGE=<repository>@sha256:<digest> cargo test --locked -p falcone-serverless-worker --test cluster_profile_javascript -- --ignored --exact cluster_profile_javascript --nocapture
```

The test checks static eligibility before constructing a cluster client.
Unset FS_TEST_CLUSTER reports UNVERIFIED. Default namespace mode creates and
deletes a dedicated fs-test-* namespace with cleanup on drop and on failure.
A creation denial fails before mutation and names FS_TEST_NAMESPACE_MODE.
Shared mode uses FS_TEST_NAMESPACE and touches only objects labelled with this
run id; it never creates or deletes a namespace. The pod has seccomp RuntimeDefault,
an arbitrary non-root UID, read-only root, no privilege escalation, dropped ALL
capabilities and a bounded scratch emptyDir. Images must be digest-pinned.
The prebuilt runner compares every recorded statusCode, headers, body and
storage/events call sequence. Any mismatch blocks the verdict. It does not
implement the missing dependency mapping or manufacture passing evidence.
No Secret values are read. Live evidence must be published by artifact ID and
SHA256 on WAS-03 after an actual run. FR-WAS-06 adversarial/leakage/recovery,
catalogue import and production enablement remain separate work.

<!-- qualification-summary: {"id":"javascript-wasi-v1","pins":{"engine":"StarlingMonkey","qualification_status":"disabled","jco":{"name":"@bytecodealliance/jco","version":"1.41.0","source":"https://registry.npmjs.org/@bytecodealliance/jco/-/jco-1.41.0.tgz","archive":"jco-1.41.0.tgz","sha256":"abe7ab13d0fb1b6ecc6f7d3fffd6ff27862d158936a11d6f48bcb0ada00af4c6","integrity":"sha512-hsl9PEj5kOQLMsUhMqaho6yYooQ8SqULjPCuUuUSXu0D3huVQKFwmbUH0B9JTBpLPZPuJJNfnpKnZx6rV53VOA=="},"componentize_js":{"name":"@bytecodealliance/componentize-js","version":"0.23.0","source":"https://registry.npmjs.org/@bytecodealliance/componentize-js/-/componentize-js-0.23.0.tgz","archive":"componentize-js-0.23.0.tgz","sha256":"b2e4b6fdee055805370950f127c21d7c549dcc7a1e907f20c5ee769ebba7ad10","integrity":"sha512-Qc2+lrwJHU7lq4CBMyVVYE8ASyUFeegMYdRnMVG0wNpNiATzGDF/mWpjDNTeVZeT8L9chhK3vuLaQE9FVANvfw=="},"wasi_virt":{"name":"wasi-virt","version":"0.2.0","source":"https://github.com/bytecodealliance/WASI-Virt","rev":"bca61c27499d2c290b5431cfcff013737b3ba01d"},"node":{"name":"Node.js","version":"v26.13.0","source":"https://nodejs.org/dist/v26.13.0/node-v26.13.0-linux-x64.tar.xz","archive":"node-v26.13.0-linux-x64.tar.xz","sha256":"d28a96af6c38596f6d9d97c0ee531c13d238d757742387d1928de822d959e997"},"wasmtime":{"version":"49.0.5"},"wit":{"name":"falcone:host","version":"0.2.0"},"wasi":{"version":"none"},"catalogue":{"revision":"unqualified","packages":{}}},"allowedInterfaces":["falcone:host/storage@0.2.0","falcone:host/http@0.2.0","falcone:host/events@0.2.0","falcone:host/workflows@0.2.0","falcone:host/logging@0.2.0","falcone:host/cancellation@0.2.0","falcone:host/progress@0.2.0","falcone:host/invocation-context@0.2.0"],"diagnostics":["WASM-NATIVE-LIBRARY","WASM-GPU","WASM-PROCESS-SPAWN","WASM-INSTALL-HOOK","WASM-PACKAGE-NOT-IN-CATALOGUE","WASM-UNSUPPORTED-API"],"verdict":"blocked","runtime_mode":"wasm"} -->
