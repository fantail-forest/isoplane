#!/usr/bin/env python3
"""Package explicitly selected Linux node binaries, their runtime profiles and
the shared runtimes of the SDK releases the node supports."""

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import tarfile
import tempfile

RUNTIME_MODULE = "Isoplane.MonoRuntime.wasm"
RUNTIME_IDENTITY = "runtime.json"


def sha256(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def install_runtimes(sources, runtimes):
    """Copy each runtime directory's module and identity into runtimes/SDK_VERSION
    and list the versions in runtimes/inventory.json."""
    versions = []
    for source in sources:
        identity = json.loads((source / RUNTIME_IDENTITY).read_text())
        version = identity["sdkVersion"]
        if sha256(source / RUNTIME_MODULE) != identity["modules"][RUNTIME_MODULE]["sha256"]:
            raise SystemExit(f"{source / RUNTIME_MODULE} is not the module its {RUNTIME_IDENTITY} names")
        if version in versions:
            raise SystemExit(f"SDK {version}'s runtime is selected twice")
        release = runtimes / version
        release.mkdir(parents=True)
        for name in (RUNTIME_MODULE, RUNTIME_IDENTITY):
            shutil.copy2(source / name, release / name)
        versions.append(version)
    (runtimes / "inventory.json").write_text(json.dumps({"sdkVersions": sorted(versions)}) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binaries", type=Path, required=True)
    parser.add_argument("--profiles", type=Path, required=True)
    parser.add_argument("--runtimes", type=Path, nargs="+", required=True,
                        help="runtime directories, each holding runtime.json and the module it names")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.output.exists():
        parser.error("Choose a new output path; release archives are immutable.")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="isoplane-node-package-") as temporary:
        stage = Path(temporary)
        shutil.copy2(args.binaries / "isoplaned", stage / "isoplaned")
        for name in ("isoplaned.service", "install-node.sh"):
            shutil.copy2(Path(__file__).with_name(name), stage / name)
        shutil.copy2(args.profiles, stage / "profiles.json")
        install_runtimes(args.runtimes, stage / "runtimes")
        files = sorted(path for path in stage.rglob("*") if path.is_file())
        (stage / "SHA256SUMS").write_text(
            "".join(f"{sha256(path)}  {path.relative_to(stage)}\n" for path in files))
        with tarfile.open(args.output, "x:gz") as archive:
            for path in sorted(stage.iterdir()):
                archive.add(path, arcname=path.name)
    args.output.with_suffix(args.output.suffix + ".sha256").write_text(
        f"{sha256(args.output)}  {args.output.name}\n")


if __name__ == "__main__":
    main()
