#!/usr/bin/env python3
"""Record what the shared runtime modules in a directory were built from.

Usage: receipt.py DIRECTORY ROOT [SOURCE_REVISION]
"""
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path

MODULES = ("Isoplane.MonoRuntime.wasm", "Isoplane.MonoRuntime.Debug.wasm")


def source_revision(root):
    try:
        revision = subprocess.check_output(["git", "-C", str(root), "rev-parse", "HEAD"], text=True).strip()
        dirty = subprocess.run(["git", "-C", str(root), "diff", "--quiet", "HEAD"]).returncode != 0
    except (OSError, subprocess.CalledProcessError):
        sys.exit("receipt: no source revision given and the source is not a git checkout")
    return revision + ("+dirty" if dirty else "")


def main(directory, root, revision=None):
    directory, root = Path(directory), Path(root)
    version = re.search(r"<Version>([^<]+)</Version>", (root / "Directory.Build.props").read_text())
    if version is None:
        sys.exit("receipt: Directory.Build.props declares no <Version>")
    packs = {(directory / f"{name}.runtime").read_text().strip() for name in MODULES}
    if len(packs) != 1:
        sys.exit(f"receipt: the runtime modules linked different Mono packs: {sorted(packs)}")
    receipt = {
        "sdkVersion": version.group(1).strip(),
        "sourceRevision": revision or source_revision(root),
        "monoPack": packs.pop(),
        "modules": {name: hashlib.sha256((directory / name).read_bytes()).hexdigest() for name in MODULES},
    }
    (directory / "runtime-build.json").write_text(json.dumps(receipt, separators=(",", ":")) + "\n")


if __name__ == "__main__":
    main(*sys.argv[1:])
