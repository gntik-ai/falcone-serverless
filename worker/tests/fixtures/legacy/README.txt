All inputs are synthesized. There are no captured tenant requests or credentials.

The default Rust suite runs only offline static/gate/oracle and WIT-mapper tests.
mapped-failure.wat is a host-mapping test, not JavaScript-engine qualification.
The checker deliberately supports a small grammar: one main(params), JSON-like
values, local declarations, return/throw, supported Web Crypto and explicit host
imports. Unknown syntax, dynamic member access and unknown calls fail closed.

Operator-only sequence, inside the project's trusted compiler image with no network:
1. Build compat_static and artifact-compile-child with cargo --offline --locked.
2. Set FS_TOOLCHAIN_CACHE to the immutable source-archive cache. Set
   FS_JS_TOOLCHAIN_ROOT to the reviewed installed toolchain (node/bin/node,
   node_modules/@bytecodealliance/{jco,componentize-js}, bin/wasi-virt and the
   wasi-virt.commit provenance file). Set FS_LEGACY_ORACLE to compat_static.
3. Run build.py --out <new scratch directory>, or the ignored build_legacy_fixtures
   test with FS_LEGACY_BUILD_OUTPUT. Source archives are checked before tool use;
   versions/commit must match versions.toml. No download, install or native fallback.
   The final component passes the unchanged worker binary import gate. The script
   writes fixed expected envelopes independently of the adapter, using inventory
   result literals and the actual component digest. No field is wildcarded.
4. Stage the repository and that output as components/ in an isolated build context.
   Build the sibling Containerfile with a reviewed digest-pinned
   TRUSTED_COMPILER_IMAGE. Record the fixture component and image artifact IDs and
   sha256 values on story #18. Do not place generated components in the repository.
5. On the authorized shared cluster, set FS_TEST_CLUSTER=1, KUBECONFIG and
   FS_TEST_WAS02_IMAGE=<fixture-image>@sha256:<digest>, then run:
   cargo test --locked -p falcone-serverless-worker --test cluster_legacy_fixtures
     -- --ignored --exact cluster_legacy_fixtures --nocapture
   Default FS_TEST_NAMESPACE_MODE=create uses a dedicated fs-test-* namespace and
   deletes it on completion or failure. A denied namespace creation fails before
   mutation and names the switch. Shared mode uses FS_TEST_NAMESPACE and the
   existing run-label ownership guard; it never creates/deletes namespaces.

The fixture image runs only the two ignored component tests. These check the
import gate, all eight inventoried legacy result paths, success, throw, async
rejection, identity/header spoofing, scoped host denial and guessed handles.
Missing/mismatched context and disabled/blocked admission are checked in the same
pod before a guest starts. Each log and envelope must compare exactly; the current
inventory declares no variable fields or removable prefixes.

Runtime evidence is UNVERIFIED until this operator sequence passes. The production
JavaScript profile remains disabled. Qualification-only entry points are compiled
only with qualification-hooks; they do not exist in the production worker.
