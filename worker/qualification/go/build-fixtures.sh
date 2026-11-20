#!/usr/bin/env bash
# Operator-run inside the project's trusted compiler image, with networking disabled.
# No fetching, installing or building toolchains. Only positive source examples are built.
set -euo pipefail
[[ ${FS_TRUSTED_COMPILER_IMAGE:-} == 1 ]] || { echo 'requires the trusted compiler image' >&2; exit 1; }
: "${FS_TOOLCHAIN_CACHE:?offline toolchain cache required}"
: "${FS_GO_COMPONENTS:?new output directory required}"
: "${TMPDIR:?run scratch directory required}"
root=$(cd "$(dirname "$0")/../../.." && pwd)
cd "$root"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$TMPDIR/go-profile-cargo}"
profile="$root/worker/qualification/profiles/go"

# Verify the exact operator archives and the trusted-image wasi-virt revision receipt
# before using any cached guest tool. The image supplies their read-only installations.
python3 - "$FS_TOOLCHAIN_CACHE" worker/qualification/go/versions.toml <<'PY'
import hashlib, pathlib, sys, tomllib
cache = pathlib.Path(sys.argv[1])
pins = tomllib.loads(pathlib.Path(sys.argv[2]).read_text())
for key in ('tinygo', 'go'):
    pin = pins[key]
    with (cache / pin['archive']).open('rb') as archive:
        digest = hashlib.file_digest(archive, 'sha256').hexdigest()
    if digest != pin['sha256']:
        raise SystemExit('toolchain archive sha256 mismatch: ' + key)
if (cache / 'wasi-virt.rev').read_text().strip() != pins['wasi_virt']['rev']:
    raise SystemExit('wasi-virt trusted-image revision mismatch')
PY
export GOROOT="$FS_TOOLCHAIN_CACHE/go"
export TINYGOROOT="$FS_TOOLCHAIN_CACHE/tinygo"
export PATH="$GOROOT/bin:$TINYGOROOT/bin:$PATH"
export GOPROXY=off GOSUMDB=off GOTOOLCHAIN=local GOWORK=off
[[ $("$GOROOT/bin/go" version) == 'go version go1.26.11 linux/amd64' ]] || { echo 'Go version mismatch' >&2; exit 1; }
[[ $("$TINYGOROOT/bin/tinygo" version) == 'tinygo version 0.44.0 '* ]] || { echo 'TinyGo version mismatch' >&2; exit 1; }
[[ ! -e "$FS_GO_COMPONENTS" ]] || { echo 'output directory already exists' >&2; exit 1; }
stage=$(mktemp -d "$TMPDIR/profile-go.XXXXXXXX")
trap 'rm -rf -- "$stage"' EXIT
mkdir "$stage/components"
export GOCACHE="$stage/go-cache" GOPATH="$stage/go-path" TINYGOCACHE="$stage/tinygo-cache"

# Static checks precede all builds. No negative source is copied into a build tree.
cargo test --offline --locked -p falcone-serverless-worker --test compat_go_static --test profile_go
for name in json-transform hash-sha256 storage-events; do
    mkdir -p "$stage/$name/bindings"
    cp "$profile/examples/$name/main.go" "$profile/examples/$name/go.mod" "$stage/$name/"
    cp "$profile/bindings/abi.go" "$stage/$name/bindings/"
    if [[ $name == storage-events ]]; then
        cp "$profile/bindings/storage.go" "$stage/$name/bindings/"
    fi
    (
        cd "$stage/$name"
        "$TINYGOROOT/bin/tinygo" build -buildmode=c-shared -target=wasip2 -wit-package "$root/worker/wit/host.wit" -wit-world function -o "$stage/$name.raw.wasm" .
    )
    "$FS_TOOLCHAIN_CACHE/bin/wasi-virt" --deny-all "$stage/$name.raw.wasm" -o "$stage/components/$name.wasm"
done

# The worker validator checks the whole set before compilation/execution. A rejected
# import (including WASI) or ABI/output mismatch stops publication of every component.
FS_GO_COMPONENTS="$stage/components" cargo test --offline --locked -p falcone-serverless-worker --test profile_go_runtime -- --ignored --exact built_profile_components --nocapture
mkdir -p "$FS_GO_COMPONENTS/bin"
cp "$stage/components/"*.wasm "$FS_GO_COMPONENTS/"
cargo test --offline --locked -p falcone-serverless-worker --test profile_go_runtime --no-run --message-format=json > "$stage/build.jsonl"
python3 - "$stage/build.jsonl" "$FS_GO_COMPONENTS/bin/profile-go-runtime" <<'PY'
import json, pathlib, shutil, sys
artifacts = [json.loads(line) for line in pathlib.Path(sys.argv[1]).read_text().splitlines()]
tests = [a['executable'] for a in artifacts if a.get('target', {}).get('name') == 'profile_go_runtime' and a.get('executable')]
if len(tests) != 1:
    raise SystemExit('missing unique profile_go_runtime test executable')
shutil.copy2(tests[0], sys.argv[2])
PY
cp "$CARGO_TARGET_DIR/debug/artifact-compile-child" "$FS_GO_COMPONENTS/bin/"
(cd "$FS_GO_COMPONENTS" && sha256sum ./*.wasm bin/* > SHA256SUMS)
