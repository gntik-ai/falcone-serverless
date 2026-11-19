# Synthetic JavaScript qualification fixtures

The fixtures contain no captured tenant data. `expected.json` uses the frozen legacy
handler and execution-failure shapes in the contract inventory and worker execution API.
No field or log prefix is implicitly variable. `compat_static` supplies independently
specified raw executor outputs and compares the normalized adapter results with all eight
inventoried legacy result fixtures. These tests stub the executor boundary; only the gated
component suite executes the JS wrapper.

The checker intentionally accepts a closed subset: a single main definition, JSON values,
read-only params/context fields, single-literal console.log, throw/rejection, the two allowed Web Crypto
operations, and the fixture's WIT storage calls. Unknown syntax and every dependency are
blocked while the package catalogue is unqualified. A supported verdict only permits
qualification. The registry still refuses all JavaScript because the matrix is disabled.

`Input::wire` supplies the original params and a separate verified context to the trusted
wrapper. Main receives `(params, context)`; tenant/workspace come from the credential, and
invocation identifiers are read through WIT. Identity-looking input fields remain input
data and never grant authority. No credential or capability binding enters the guest.

The operator runs `bash worker/qualification/javascript/build-fixtures.sh` only in the
offline, digest-pinned trusted compiler image, with:

- `FS_TRUSTED_COMPILER_IMAGE=1` as the explicit operator opt-in;
- `FS_TOOLCHAIN_CACHE` containing the exact archives in versions.toml, preinstalled
  `bin/node`, `bin/wasi-virt`, the npm dependency closure under node_modules, and the
  image-build receipt `wasi-virt.rev` containing the approved git revision;
- `FS_LEGACY_COMPONENTS` naming a new output directory, and `TMPDIR` naming scratch space.

The environment opt-in is not image authentication: the operator must select the trusted
compiler image by digest and its build provenance. The script checks archive hashes and
versions, builds no toolchain, disables optional WASI features, applies deny-all WASI
virtualization, and executes the raw components through the unchanged worker import gate.
An unsupported toolchain option, leftover WASI import or fixture mismatch fails the build.

The script produces components, a test runner, the trusted compiler child and SHA256SUMS.
Record their artifact ID and SHA256 on the story before considering enablement. Runtime
qualification is UNVERIFIED until that operator evidence exists.

`denied.js` exercises the host's storage-prefix check. `guessed.js` supplies a number where
WIT requires a borrowed object resource, so it may fail during guest resource lowering.
Its error envelope and absence of platform calls demonstrate boundary rejection, not
evidence that a guessed handle reached and was denied by the host. Host-side handle
enforcement remains SEC-03's separate qualification responsibility.

The offline `legacy_runtime::host_storage_scope_denials_reach_the_adapter_as_inventory_failures`
test also runs a WAT probe (`host-storage.wat`) through the real WIT storage host and the
adapter. An authorized read succeeds and reaches the platform binding; forged tenant and
workspace paths receive `host-error.denied`, reach no platform binding, and become the
inventoried failure envelope. It verifies host denial and adapter error mapping without a
JS toolchain; it does not qualify the JS wrapper or guessed-resource lowering.

For the cluster qualification image, copy bin/legacy-runtime and bin/artifact-compile-child
to /usr/local/bin, and the raw components to /opt/falcone-toolchain-cache/legacy. Supply this
image by digest as FS_TEST_WAS02_IMAGE. With FS_TEST_CLUSTER=1, FS_TOOLCHAIN_CACHE set as an
explicit qualification opt-in, and the existing harness's KUBECONFIG and namespace inputs,
run `cargo test --locked -p falcone-serverless-worker --test cluster_legacy_fixtures -- --include-ignored --nocapture`.
The suite uses the existing restricted-pod and run-labelled cleanup harness. Shared namespace
mode follows the existing ownership rules; default mode creates and deletes fs-test-*.
