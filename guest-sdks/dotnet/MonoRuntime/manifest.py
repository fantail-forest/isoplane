#!/usr/bin/env python3
"""Write the runtime identity shipped in SDK and application packages."""
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path

MODULES = ("Isoplane.MonoRuntime.wasm", "Isoplane.MonoRuntime.Debug.wasm")
PROVENANCE = "runtime.json"


def source_revision(root):
    try:
        revision = subprocess.check_output(["git", "-C", str(root), "rev-parse", "HEAD"], text=True).strip()
        dirty = subprocess.run(["git", "-C", str(root), "diff", "--quiet", "HEAD"]).returncode != 0
    except (OSError, subprocess.CalledProcessError):
        sys.exit("runtime manifest: no source revision given and the source is not a git checkout")
    return revision + ("+dirty" if dirty else "")


def provenance(receipt):
    """The runtime's identity as the SDK package and the platform read it."""
    version, sha512 = receipt["monoPack"].split("@sha512:", 1)
    return {
        "sdkVersion": receipt["sdkVersion"],
        "sourceRevision": receipt["sourceRevision"],
        "monoPack": {"version": version, "sha512": sha512},
        "modules": {name: {"sha256": digest} for name, digest in receipt["modules"].items()},
    }


def main(directory, root, revision=None):
    directory, root = Path(directory), Path(root)
    version = re.search(r"<Version>([^<]+)</Version>", (root / "Directory.Build.props").read_text())
    if version is None:
        sys.exit("runtime manifest: Directory.Build.props declares no <Version>")
    packs = {(directory / f"{name}.runtime").read_text().strip() for name in MODULES}
    if len(packs) != 1:
        sys.exit(f"runtime manifest: the runtime modules linked different Mono packs: {sorted(packs)}")
    receipt = {
        "sdkVersion": version.group(1).strip(),
        "sourceRevision": revision or source_revision(root),
        "monoPack": packs.pop(),
        "modules": {name: hashlib.sha256((directory / name).read_bytes()).hexdigest() for name in MODULES},
    }
    (directory / PROVENANCE).write_text(json.dumps(provenance(receipt), separators=(",", ":")) + "\n")


if __name__ == "__main__":
    main(*sys.argv[1:])
