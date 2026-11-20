#!/usr/bin/env bash
# Operator-run only, inside the offline trusted compiler image.
set -euo pipefail
[[ ${FS_TRUSTED_COMPILER_IMAGE:-} == 1 ]] || { echo 'requires the trusted compiler image' >&2; exit 1; }
: "${FS_TOOLCHAIN_CACHE:?offline toolchain cache required}"
: "${FS_PROFILE_COMPONENTS:?new output directory required}"
: "${TMPDIR:?run scratch directory required}"
root=$(cd "$(dirname "$0")/../../.." && pwd)
cd "$root"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$TMPDIR/profile-go-cargo}"
profile="$root/worker/qualification/profiles/go"

# Verify the operator's archives before executing any cached tool. Also compare every
# installed file to its archive; a version string alone cannot authenticate an installation.
# No downloads, installs, extraction or toolchain builds take place here.
python3 - "$FS_TOOLCHAIN_CACHE" worker/qualification/go/versions.toml worker/qualification/javascript/versions.toml <<'PY'
import hashlib, pathlib, sys, tarfile, tomllib
cache = pathlib.Path(sys.argv[1]).resolve()
pins = tomllib.loads(pathlib.Path(sys.argv[2]).read_text())
for key in ('tinygo', 'go'):
    pin = pins[key]
    archive = cache / pin['archive']
    with archive.open('rb') as stream:
        if hashlib.file_digest(stream, 'sha256').hexdigest() != pin['sha256']:
            raise SystemExit('toolchain archive sha256 mismatch: ' + key)
    expected = set()
    with tarfile.open(archive, 'r:gz') as tar:
        for member in tar:
            name = pathlib.PurePosixPath(member.name)
            if name.is_absolute() or '..' in name.parts or name.parts[0] != key:
                raise SystemExit('unexpected toolchain archive path')
            target = cache / name
            if member.isdir():
                continue
            expected.add(name.as_posix())
            if member.issym():
                if not target.is_symlink() or target.readlink().as_posix() != member.linkname:
                    raise SystemExit('installed toolchain symlink mismatch: ' + key)
                if not target.resolve().is_relative_to(cache / key):
                    raise SystemExit('toolchain symlink escapes pinned installation')
            elif member.isfile():
                if target.is_symlink() or not target.is_file():
                    raise SystemExit('installed toolchain file missing: ' + key)
                with tar.extractfile(member) as a, target.open('rb') as b:
                    if hashlib.file_digest(a, 'sha256').digest() != hashlib.file_digest(b, 'sha256').digest():
                        raise SystemExit('installed toolchain file mismatch: ' + key)
            else:
                raise SystemExit('unsupported toolchain archive member')
    actual = {p.relative_to(cache).as_posix() for p in (cache / key).rglob('*') if not p.is_dir() or p.is_symlink()}
    if actual != expected:
        raise SystemExit('unpinned files in installed toolchain: ' + key)
# Reuse the already recorded trusted wasi-virt revision; never relax the final import gate.
virt = tomllib.loads(pathlib.Path(sys.argv[3]).read_text())['wasi_virt']
if (cache / 'wasi-virt.rev').read_text().strip() != virt['rev']:
    raise SystemExit('wasi-virt trusted-image revision mismatch')
PY
[[ ! -e "$FS_PROFILE_COMPONENTS" ]] || { echo 'output directory already exists' >&2; exit 1; }
stage=$(mktemp -d "$TMPDIR/profile-go.XXXXXXXX")
trap 'rm -rf -- "$stage"' EXIT
mkdir "$stage/components"
export GOROOT="$FS_TOOLCHAIN_CACHE/go"
export TINYGOROOT="$FS_TOOLCHAIN_CACHE/tinygo"
export PATH="$GOROOT/bin:$PATH"
export GOPROXY=off GOSUMDB=off GOTOOLCHAIN=local GOENV=off CGO_ENABLED=0
export GOCACHE="$stage/go-cache" GOPATH="$stage/gopath"
[[ $("$GOROOT/bin/go" version) == 'go version go1.26.11 linux/amd64' ]] || { echo 'Go version mismatch' >&2; exit 1; }
[[ $("$TINYGOROOT/bin/tinygo" version) == 'tinygo version 0.44.0 '* ]] || { echo 'TinyGo version mismatch' >&2; exit 1; }

# Static checks precede builds. The negative directory is never passed to a compiler.
cargo test --offline --locked -p falcone-serverless-worker --test profile_go --test language_profile_matrix
for name in echo-json uppercase hash-sha256; do
    (cd "$profile" && "$TINYGOROOT/bin/tinygo" build -target=wasip2 -buildmode=c-shared \
        -wit-package "$root/worker/wit" -wit-world function \
        -o "$stage/$name.raw.wasm" "./examples/$name")
    "$FS_TOOLCHAIN_CACHE/bin/wasi-virt" --deny-all "$stage/$name.raw.wasm" -o "$stage/components/$name.wasm"
done
FS_PROFILE_COMPONENTS="$stage/components" cargo test --offline --locked -p falcone-serverless-worker \
    --test profile_go -- --ignored --exact trusted_components_have_one_allocator_and_only_declared_imports --nocapture

# Ship a prebuilt runtime test binary and its trusted compiler child for the restricted pod.
# This script parses components but never executes a guest; execution is cluster-gated.
cargo test --offline --locked -p falcone-serverless-worker --test profile_go_runtime --no-run --message-format=json > "$stage/build.jsonl"
mkdir "$stage/components/bin"
python3 - "$stage/build.jsonl" "$stage/components/bin/profile-go-runtime" <<'PY'
import json, pathlib, shutil, sys
artifacts = [json.loads(line) for line in pathlib.Path(sys.argv[1]).read_text().splitlines()]
tests = [a['executable'] for a in artifacts if a.get('target', {}).get('name') == 'profile_go_runtime' and a.get('executable')]
if len(tests) != 1:
    raise SystemExit('missing unique profile_go_runtime test executable')
shutil.copy2(tests[0], sys.argv[2])
PY
cp "$CARGO_TARGET_DIR/debug/artifact-compile-child" "$stage/components/bin/"
(cd "$stage/components" && sha256sum ./*.wasm bin/* > SHA256SUMS)
mv "$stage/components" "$FS_PROFILE_COMPONENTS"
