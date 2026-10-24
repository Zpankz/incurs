"""Build the pinned dquickts engine for the Cloudflare checkpoint executor."""

import argparse
import hashlib
import json
import pathlib
import shutil
import subprocess

ROOT = pathlib.Path(__file__).resolve().parent
REVISION = "f9a3efaa4d7cadb9dcad7e80a03d2774eb95a406"
SOURCE = ROOT.parent.parent / "crates/incurs-codemode-local/native/quickjs"
DEFAULT_OUTPUT = ROOT / "crates/incurs-codemode-cloudflare/src/dquickts"


def run(*args):
    subprocess.run(args, check=True)


def verify_source(source):
    provenance_path = source / "provenance.json"
    if provenance_path.exists():
        provenance = json.loads(provenance_path.read_text())
        verify_vendor(source, provenance)
        return
    if not (source / ".git").exists():
        source.mkdir(parents=True, exist_ok=True)
        run("git", "-C", str(source), "init", "--quiet")
        run("git", "-C", str(source), "fetch", "--depth=1",
            "https://github.com/stencil-hq/dquickts", REVISION)
        run("git", "-C", str(source), "checkout", "--detach", "FETCH_HEAD")
    revision = subprocess.check_output(["git", "-C", str(source), "rev-parse", "HEAD"], text=True).strip()
    if revision != REVISION:
        raise SystemExit(f"Expected dquickts {REVISION}, got {revision}")
    if subprocess.check_output(["git", "-C", str(source), "status", "--porcelain"], text=True).strip():
        raise SystemExit("dquickts source must be clean")


def verify_vendor(source, provenance):
    if provenance["revision"] != REVISION:
        raise SystemExit("Vendored engine revision differs from the pinned revision")
    for name, digest in provenance["sha256"].items():
        if hashlib.sha256((source / name).read_bytes()).hexdigest() != digest:
            raise SystemExit(f"Vendored engine file changed: {name}")


def compile_engine(source, output):
    output.mkdir(parents=True, exist_ok=True)
    version = (source / "VERSION").read_text().strip()
    exported = ["_cm_" + name for name in ["alloc", "create", "restore", "pump", "state", "respond", "snapshot", "snapshot_size", "error", "destroy"]] + ["_malloc", "_free"]
    run("emcc", "-O2", "-fwrapv", "-funsigned-char", "-D_GNU_SOURCE",
        f'-DCONFIG_VERSION="{version}"', "-I", str(source),
        str(SOURCE.parent / "bridge.c"),
        *[str(source / name) for name in ["quickjs.c", "libregexp.c", "libunicode.c", "cutils.c", "dtoa.c"]],
        "--no-entry", "-s", "MODULARIZE=1", "-s", "EXPORT_ES6=1",
        "-s", "ENVIRONMENT=web,worker,node", "-s", "ALLOW_MEMORY_GROWTH=1",
        "-s", "MAXIMUM_MEMORY=134217728", "-s", "STACK_SIZE=1048576",
        "-s", "EXPORTED_FUNCTIONS=" + json.dumps(exported),
        "-s", 'EXPORTED_RUNTIME_METHODS=["UTF8ToString","stringToNewUTF8","HEAPU8"]',
        "-o", str(output / "engine.js"))
    shutil.copyfile(source / "LICENSE", output / "engine.LICENSE")
    artifacts = {name: hashlib.sha256((output / name).read_bytes()).hexdigest()
                 for name in ["engine.js", "engine.wasm", "engine.LICENSE"]}
    manifest = {"revision": REVISION, "compiler": subprocess.check_output(["emcc", "--version"], text=True).splitlines()[0],
                "bridge_sha256": hashlib.sha256((SOURCE.parent / "bridge.c").read_bytes()).hexdigest(),
                "sha256": artifacts}
    (output / "provenance.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps(manifest))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=pathlib.Path, default=SOURCE)
    parser.add_argument("--output", type=pathlib.Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--check", action="store_true", help="Verify checked-in source and engine hashes without compiling.")
    args = parser.parse_args()
    verify_source(args.source)
    if args.check:
        verify_artifacts(args.output)
    else:
        compile_engine(args.source, args.output)


def verify_artifacts(output):
    manifest = json.loads((output / "provenance.json").read_text())
    if manifest["revision"] != REVISION:
        raise SystemExit("Engine artifact revision differs from the pinned revision")
    expected = manifest["bridge_sha256"]
    if hashlib.sha256((SOURCE.parent / "bridge.c").read_bytes()).hexdigest() != expected:
        raise SystemExit("Engine bridge changed; rebuild the WebAssembly engine")
    for name, digest in manifest["sha256"].items():
        if hashlib.sha256((output / name).read_bytes()).hexdigest() != digest:
            raise SystemExit(f"Engine artifact changed: {name}")
    print(json.dumps({"verdict": "pass", "revision": REVISION}))


if __name__ == "__main__":
    main()
