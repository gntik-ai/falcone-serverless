#!/usr/bin/env python3
"""Operator-only, offline fixture build inside the project's trusted compiler image.

The image provides the installed, reviewed toolchain; archives in the immutable cache
are verified before any tool runs. This script never installs or downloads anything.
"""
import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tomllib


def fail(message):
    raise RuntimeError(message)


def run(args, **kwargs):
    result = subprocess.run(args, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, **kwargs)
    if result.returncode:
        fail("fixture tool failed")
    return result.stdout


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--verify-only", action="store_true")
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()
    here = Path(__file__).resolve().parent
    repo = here.parents[3]
    pins = tomllib.loads((repo / "worker/qualification/javascript/versions.toml").read_text())
    cache = os.environ.get("FS_TOOLCHAIN_CACHE")
    if not cache:
        fail("UNVERIFIED: FS_TOOLCHAIN_CACHE unset")
    for name in ("jco", "componentize_js", "node"):
        pin = pins[name]
        archive = Path(cache) / pin["archive"]
        if not archive.is_file():
            fail("toolchain archive missing: " + name)
        with archive.open("rb") as stream:
            digest = hashlib.file_digest(stream, "sha256").hexdigest()
        if digest != pin["sha256"]:
            fail("toolchain sha256 mismatch: " + name)
    if args.verify_only:
        return
    if not args.out or args.out.exists():
        fail("--out must name a new scratch directory")
    tools = os.environ.get("FS_JS_TOOLCHAIN_ROOT")
    oracle = os.environ.get("FS_LEGACY_ORACLE")
    if not tools or not oracle:
        fail("UNVERIFIED: FS_JS_TOOLCHAIN_ROOT and FS_LEGACY_ORACLE required")
    tools = Path(tools).resolve()
    for name, directory in (("jco", "jco"), ("componentize_js", "componentize-js")):
        package = json.loads((tools / "node_modules/@bytecodealliance" / directory / "package.json").read_text())
        if package["name"] != pins[name]["name"] or package["version"] != pins[name]["version"]:
            fail("installed toolchain version mismatch")
    if (tools / "wasi-virt.commit").read_text().strip() != pins["wasi_virt"]["commit"]:
        fail("wasi-virt commit mismatch")
    node = tools / "node/bin/node"
    if run([node, "--version"]).decode().strip() != pins["node"]["version"]:
        fail("installed Node version mismatch")
    jco_package = tools / "node_modules/@bytecodealliance/jco"
    jco = jco_package / json.loads((jco_package / "package.json").read_text())["bin"]["jco"]
    # This executable is the local compat_static suite built with qualification hooks.
    checked = run([oracle, "--exact", "synthesized_legacy_sources_have_no_static_blockers"])
    if b"1 passed; 0 failed" not in checked:
        fail("fixture source checker did not run")
    cases = json.loads((here / "cases.json").read_text())
    inventory = repo / "docs/contracts/fixtures/legacy-envelope/valid"
    for path in sorted(inventory.glob("*.json")):
        fixture = json.loads(path.read_text())
        cases["inventory-" + path.stem] = {"http_status": 200, "status": "success", "result": fixture["expected"], "logs": [], "inventory_params": fixture["params"]}
    args.out.mkdir(parents=True)
    records = []
    request_template = json.loads((repo / "worker/tests/fixtures/contract/request/valid/wasm-no-base.json").read_text())
    failure = json.loads((repo / "worker/tests/fixtures/contract/result/valid/failure-execution-failed.json").read_text())["result"]["result"]
    for name, case in cases.items():
        directory = args.out / name
        directory.mkdir()
        source = here / name / "function.js" if not name.startswith("inventory-") else here / "inventory-main.js"
        legacy = source.read_text()
        (directory / "legacy.js").write_text(legacy)
        # The static grammar accepts a legacy script declaration as well as an ESM export.
        # Preserve the original source for the oracle and export its main for trusted glue.
        if not re.search(r"\bexport\s+(?:async\s+)?function\s+main\b", legacy):
            legacy += "\nexport { main };\n"
        (directory / "function.js").write_text(legacy)
        shutil.copyfile(here / "bridge.js", directory / "bridge.js")
        # No ambient WASI feature is available to the final component.
        run([node, jco, "componentize", directory / "bridge.js", "--wit", repo / "worker/wit/host.wit", "--world-name", "function", "--disable", "all", "--out", directory / "raw.wasm"])
        run([tools / "bin/wasi-virt", "--deny-all", "-o", directory / "component.wasm", directory / "raw.wasm"])
        component = (directory / "component.wasm").read_bytes()
        digest = "sha256:" + hashlib.sha256(component).hexdigest()
        request = copy.deepcopy(request_template)
        request["projection"]["artifact_digest"] = digest
        request["projection"]["component_digest"] = digest
        request["projection"]["language_profile"] = {"name": "javascript-wasi-v1", "version": "1.41.0"}
        request["projection"]["imports"] = ["falcone:host/logging@0.2.0", "falcone:host/invocation-context@0.2.0", "falcone:host/events@0.2.0", "falcone:host/storage@0.2.0"]
        request["credential"]["allowed_capabilities"].append({"family": "event", "interface": "falcone:host/events@0.1.0", "scope": {"topics": ["orders"]}})
        request["projection"]["grants"].append({"family": "event", "interface": "falcone:host/events@0.2.0", "scope": {"topics": ["orders"]}})
        if name.startswith("inventory-"):
            request["params"] = case["inventory_params"]
            request["params"]["fixture_result"] = case["result"]
        # These fields stay untrusted guest inputs, even when a header spells an identity.
        if name == "identity":
            for key in ("tenant", "workspace", "invocation_key", "attempt_id", "idempotency_key"):
                request["params"][key] = "spoofed-" + key
            request["params"]["__ow_headers"] = {"x-auth-tenant-id": "spoofed-tenant", "x-workspace-id": "spoofed-workspace", "invocation-key": "spoofed-invocation", "attempt-id": "spoofed-attempt", "idempotency-key": "spoofed-idempotency"}
        expected = {"version": "1", "type": "result", "activation_id": request["activation_id"], "attempt_id": request["attempt_id"], "attempt_number": 1, "status": case["status"], "logs": case["logs"], "identity": {
            "function_revision": request["projection"]["revision"], "runtime_mode": "wasm", "artifact_digest": digest, "wasmtime_version": "49.0.5", "language_profile": request["projection"]["language_profile"], "interface_version": "0.2.0"}}
        if case["status"] == "success":
            expected["result"] = case["result"]
        else:
            expected["result"] = copy.deepcopy(failure)
            expected["result"]["detail"]["reason"] = case["reason"]
            expected["class"] = "non_retryable"
        for filename, value in (("request.json", request), ("expected.json", expected)):
            (directory / filename).write_text(json.dumps(value, indent=2) + "\n")
        records.append({"name": name, "http_status": case["http_status"], "sha256": digest})
        (directory / "raw.wasm").unlink()
    (args.out / "manifest.json").write_text(json.dumps(records, indent=2) + "\n")
    # Use the worker's unchanged binary import gate, not a textual WIT approximation.
    env = dict(os.environ, FS_LEGACY_COMPONENTS=str(args.out.resolve()))
    checked = run([oracle, "--exact", "built_components_pass_import_gate", "--ignored"], env=env)
    if b"1 passed; 0 failed" not in checked:
        fail("component import validation did not run")
    print("fixture components built; runtime qualification still required")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, ValueError, KeyError):
        error = sys.exception()
        print(str(error) if isinstance(error, RuntimeError) else "fixture inputs invalid or unavailable", file=sys.stderr)
        sys.exit(1)
