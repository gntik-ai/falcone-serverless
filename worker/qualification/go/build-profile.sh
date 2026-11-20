#!/usr/bin/env bash
# Operator-only, in the digest-pinned trusted compiler image with networking disabled.
# Uses only preinstalled pinned tools. Never downloads, installs or builds a toolchain.
set -euo pipefail
[[ ${FS_TRUSTED_COMPILER_IMAGE:-} == 1 ]] || { echo 'requires the trusted compiler image' >&2; exit 1; }
: "${FS_TOOLCHAIN_CACHE:?offline toolchain cache required}"
: "${FS_PROFILE_COMPONENTS:?new output directory required}"
root=$(cd "$(dirname "$0")/../../.." && pwd)
cd "$root"
profile=worker/qualification/profiles/go

# Validate bytes before any toolchain executable is invoked. The trusted image supplies
# the extracted tools from exactly these archives; a missing archive or mismatch stops here.
wit_world=$(python3 - "$FS_TOOLCHAIN_CACHE" worker/qualification/go/versions.toml "$profile/manifest.toml" <<'PY'
import hashlib, pathlib, sys, tomllib
cache = pathlib.Path(sys.argv[1])
pins = tomllib.loads(pathlib.Path(sys.argv[2]).read_text())
for name in ('tinygo', 'go'):
    pin = pins[name]
    archive = cache / pin['archive']
    if not archive.is_file() or hashlib.file_digest(archive.open('rb'), 'sha256').hexdigest() != pin['sha256']:
        raise SystemExit('toolchain archive sha256 mismatch: ' + name)
print(tomllib.loads(pathlib.Path(sys.argv[3]).read_text())['wit']['world'])
PY
)
: "${TMPDIR:?run scratch directory required}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$TMPDIR/profile-go-cargo}"
export GOROOT="$FS_TOOLCHAIN_CACHE/go"
export TINYGOROOT="$FS_TOOLCHAIN_CACHE/tinygo"
export PATH="$GOROOT/bin:$TINYGOROOT/bin:$PATH"
export GOPROXY=off GOSUMDB=off GOTOOLCHAIN=local
[[ $("$GOROOT/bin/go" version) == 'go version go1.26.11 linux/amd64' ]] || { echo 'Go version mismatch' >&2; exit 1; }
version=$("$TINYGOROOT/bin/tinygo" version)
[[ $version == 'tinygo version 0.44.0 '* ]] || { echo 'TinyGo version mismatch' >&2; exit 1; }
[[ ! -e "$FS_PROFILE_COMPONENTS" ]] || { echo 'output directory already exists' >&2; exit 1; }
stage=$(mktemp -d "$TMPDIR/profile-go.XXXXXXXX")
trap 'rm -rf -- "$stage"' EXIT
export GOCACHE="$stage/go-cache" GOMODCACHE="$stage/module-cache"
mkdir -p "$stage/components/evidence" "$stage/source" "$stage/components/bin"
cp -r "$profile/". "$stage/components/evidence/"
# Stage only positive source packages and the reviewed platform binding. Negative sources
# remain evidence and can never enter a compiler command.
cp "$profile/go.mod" "$stage/source/"
cp -r "$profile/bindings" "$profile/examples" "$stage/source/"
cargo test --offline --locked -p falcone-serverless-worker --test profile_go --test language_profile_matrix
for name in json-transform text-regex hash-sha256; do
    (
        cd "$stage/source"
        "$TINYGOROOT/bin/tinygo" build -target=wasip2 -buildmode=c-shared -panic=trap -scheduler=none \
            -wit-package="$root/$profile/wit" -wit-world="$wit_world" \
            -o "$stage/components/$name.wasm" "./examples/$name"
    )
done
# Build only project code, using locked cached Rust crates. These executables run in the
# restricted pod; toolchains and network access are not needed there.
cargo build --offline --locked -p falcone-serverless-worker --bin artifact-compile-child --bin compile-cache-qualification
cargo test --offline --locked -p falcone-serverless-worker --test profile_go_runtime --no-run --message-format=json > "$stage/build.jsonl"
python3 - "$stage/build.jsonl" "$stage/components/bin/profile-go-runtime" <<'PY'
import json, pathlib, shutil, sys
artifacts = [json.loads(line) for line in pathlib.Path(sys.argv[1]).read_text().splitlines()]
tests = [a['executable'] for a in artifacts if a.get('target', {}).get('name') == 'profile_go_runtime' and a.get('executable')]
if len(tests) != 1:
    raise SystemExit('missing unique profile_go_runtime test executable')
shutil.copy2(tests[0], sys.argv[2])
PY
cp "$CARGO_TARGET_DIR/debug/artifact-compile-child" "$CARGO_TARGET_DIR/debug/compile-cache-qualification" "$stage/components/bin/"
python3 - "$stage/components" <<'PY'
import hashlib, pathlib, sys
root = pathlib.Path(sys.argv[1])
files = sorted(path for path in root.rglob('*') if path.is_file())
(root / 'SHA256SUMS').write_text(''.join(hashlib.file_digest(path.open('rb'), 'sha256').hexdigest() + '  ' + path.relative_to(root).as_posix() + '\n' for path in files))
PY
# Every fixture must pass the exact import gate, one-memory/one-realloc assertions and
# Host execution under memories(1) before the bundle is published to the new directory.
FS_PROFILE_COMPONENTS="$stage/components" cargo test --offline --locked -p falcone-serverless-worker --test profile_go_runtime -- --ignored --exact built_profile_components --nocapture
# mkdir is exclusive: a race never permits overwriting an existing output.
mkdir "$FS_PROFILE_COMPONENTS"
cp -r "$stage/components/". "$FS_PROFILE_COMPONENTS/"
