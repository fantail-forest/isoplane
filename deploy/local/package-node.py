#!/usr/bin/env python3
"""Package explicitly selected Linux node binaries and their runtime profiles."""

import argparse
import hashlib
from pathlib import Path
import shutil
import tarfile
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binaries", type=Path, required=True)
    parser.add_argument("--profiles", type=Path, required=True)
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
        checksums = []
        for path in sorted(stage.iterdir()):
            with path.open("rb") as source:
                digest = hashlib.file_digest(source, "sha256").hexdigest()
            checksums.append(f"{digest}  {path.name}\n")
        (stage / "SHA256SUMS").write_text("".join(checksums))
        with tarfile.open(args.output, "x:gz") as archive:
            for path in sorted(stage.iterdir()):
                archive.add(path, arcname=path.name)
    with args.output.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    args.output.with_suffix(args.output.suffix + ".sha256").write_text(f"{digest}  {args.output.name}\n")


if __name__ == "__main__":
    main()
