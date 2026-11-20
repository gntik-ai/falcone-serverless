#!/usr/bin/env python3
"""Stage a build-only world from the already authenticated TinyGo WIT bundle.

This copies text only; it never fetches, installs or executes a toolchain. The
caller must authenticate the pinned installation before invoking TinyGo.
"""
import pathlib
import re
import shutil
import sys

tinygo, host, output = map(pathlib.Path, sys.argv[1:])
package_pattern = re.compile(r"^\s*package\s+(wasi:[a-z0-9-]+@[0-9][a-zA-Z0-9.+-]*)\s*;", re.M)
directories = {path.parent for path in tinygo.rglob("*.wit")}
cli = []
for directory in sorted(directories):
    text = "\n".join(path.read_text() for path in sorted(directory.glob("*.wit")))
    packages = set(package_pattern.findall(text))
    if len(packages) == 1 and next(iter(packages)).startswith("wasi:cli@"):
        if re.search(r"\bworld\s+imports\s*\{", text):
            cli.append((directory, next(iter(packages))))
if len(cli) != 1:
    raise SystemExit("expected one bundled TinyGo wasi:cli/imports WIT world")
cli_root, cli_package = cli[0]

# Flatten the bundled dependency packages, preserving their exact versions and
# contents. No WASI declaration is added to the runtime host world.
output.mkdir()
deps = output / "deps"
deps.mkdir()
for directory in sorted(directories):
    if not directory.is_relative_to(cli_root):
        continue
    sources = sorted(directory.glob("*.wit"))
    packages = set(package_pattern.findall("\n".join(path.read_text() for path in sources)))
    if len(packages) != 1:
        raise SystemExit("expected one WASI package per bundled WIT directory")
    package = next(iter(packages))
    destination = deps / package.replace(":", "-").replace("@", "-")
    destination.mkdir()
    for source in sources:
        shutil.copyfile(source, destination / source.name)
(deps / "falcone").mkdir()
shutil.copyfile(host, deps / "falcone" / "host.wit")
(output / "world.wit").write_text(
    "package falcone:go-build;\n\nworld function {\n"
    "    include falcone:host/function@0.2.0;\n"
    f"    include wasi:cli/imports@{cli_package.split('@')[1]};\n"
    "}\n"
)
