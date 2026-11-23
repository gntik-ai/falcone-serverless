"""Exercise the trusted build's archive boundary with inert executable fixtures.

No Go or TinyGo toolchain is installed or run. The stand-ins only check the
option-probe contract, including its module and staged WIT dependencies.
"""
import hashlib
import io
import os
import pathlib
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib


def archive(path, members):
    with tarfile.open(path, "w:gz") as output:
        for name, text in members:
            data = text.encode()
            member = tarfile.TarInfo(name)
            member.mode = 0o755
            member.size = len(data)
            output.addfile(member, io.BytesIO(data))


def run(source, scratch):
    root = scratch / "repo"
    cache = scratch / "cache"
    cache.mkdir()
    for name in ["qualification/go", "qualification/profiles/go/wit", "wit"]:
        shutil.copytree(source / "worker" / name, root / "worker" / name)
    pins_path = source / "worker/src/qualification/go_versions.toml"
    pins_text = pins_path.read_text()
    pins = tomllib.loads(pins_text)
    tools = {
        "tinygo": [("tinygo/bin/tinygo", '''#!/usr/bin/env python3
import os, pathlib, subprocess, sys
if sys.argv[1:] == ['version']:
    print('tinygo version 0.44.0 linux/amd64 (using go version go1.26.11)')
    sys.exit(0)
if pathlib.Path('go.mod').read_text() != 'module probe\\n\\ngo 1.26\\n':
    sys.exit('go.mod file not found or invalid probe module')
args = sys.argv[1:]
world = pathlib.Path(args[args.index('-wit-package') + 1])
assert (world / 'function.wit').is_file(), 'qualification world missing'
assert (world / 'deps/falcone-host/host.wit').is_file(), 'host dependency missing'
assert args[args.index('-wit-world') + 1] == 'function'
assert '-target=wasip2' in args and '-buildmode=c-shared' in args
assert args[-1] == './missing-probe-package'
assert pathlib.Path(os.environ['TINYGOROOT']).parent.name == 'tinygo'
assert pathlib.Path(os.environ['GOROOT']).parent.name == 'go'
assert subprocess.check_output(['wasm-tools', '--version'], text=True).strip() == 'wasm-tools 1.258.3'
# TinyGo runs $WASMTOOLS before PATH: it must name the verified componentizer.
assert subprocess.check_output([os.environ['WASMTOOLS'], '--version'], text=True).strip() == 'wasm-tools 1.258.3'
# TinyGo runs $WASMOPT before its bundled optimizer: it must name the verified archive's.
assert pathlib.Path(os.environ['WASMOPT']) == pathlib.Path(os.environ['TINYGOROOT']) / 'bin/wasm-opt'
assert subprocess.check_output([os.environ['WASMOPT'], '--version'], text=True).strip() == 'wasm-opt verified'
sys.exit('missing-probe-package: expected loader failure')
'''), ("tinygo/bin/wasm-opt", "#!/bin/sh\necho 'wasm-opt verified'\n")],
        "go": [("go/bin/go", "#!/bin/sh\necho 'go version go1.26.11 linux/amd64'\n")],
        "wasm_tools": [(
            "wasm-tools-1.258.3-x86_64-linux/wasm-tools",
            "#!/bin/sh\necho 'wasm-tools 1.258.3'\n",
        )],
    }
    for key, members in tools.items():
        path = cache / pins[key]["archive"]
        archive(path, members)
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        pins_text = pins_text.replace(pins[key]["sha256"], digest)
    target = root / "worker/src/qualification/go_versions.toml"
    target.parent.mkdir(parents=True)
    target.write_text(pins_text)
    # These unverified extracted files must never be used, even with valid archives.
    for name in ["bin/wasm-tools", "bin/wasm-opt", "go/bin/go", "tinygo/bin/tinygo"]:
        path = cache / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("#!/bin/sh\necho 'unverified extracted executable was used' >&2\nexit 91\n")
        path.chmod(0o755)
    env = dict(os.environ, FS_TRUSTED_COMPILER_IMAGE="1", FS_OPTION_PROBE_ONLY="1",
               FS_TOOLCHAIN_CACHE=str(cache), FS_PROFILE_COMPONENTS=str(scratch / "components"),
               TMPDIR=str(scratch),
               # Poisoned inherited overrides: the build must replace them.
               WASMTOOLS=str(cache / "bin/wasm-tools"), WASMOPT=str(cache / "bin/wasm-opt"))
    command = ["bash", str(root / "worker/qualification/go/build-profile.sh")]
    result = subprocess.run(command, env=env, text=True, capture_output=True)
    assert result.returncode == 0, result.stdout + result.stderr
    # Corruption must be caught before any stand-in tool is invoked.
    with (cache / pins["tinygo"]["archive"]).open("ab") as output:
        output.write(b"corrupt")
    result = subprocess.run(command, env=env, text=True, capture_output=True)
    assert result.returncode != 0
    assert "toolchain archive sha256 mismatch: tinygo" in result.stderr, result.stderr
    assert "unverified extracted executable" not in result.stderr


with tempfile.TemporaryDirectory(prefix="go-tool-probe-", dir=os.environ["TMPDIR"]) as scratch:
    run(pathlib.Path(sys.argv[1]), pathlib.Path(scratch))
