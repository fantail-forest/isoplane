#!/usr/bin/env python3
"""Prepare shared fixtures before starting parallel test binaries."""

import os
from pathlib import Path
import subprocess
import sys

import build_storage as storage

ROOT = Path(__file__).resolve().parent.parent


def main():
    with storage.development(ROOT, os.environ) as environment:
        for script in ("guests/build.sh", "guests/build-dotnet.sh",
                       "guest-runtime/tests/guest/build.sh", "guest-runtime/tests/packages/build.sh"):
            subprocess.run([sys.executable, str(ROOT / "scripts/dev"), "bash", str(ROOT / script), "--if-stale"],
                           cwd=ROOT, env=environment, check=True)
        for tool in ("isoplane-cli", "debugger-protocol", "dap-python"):
            subprocess.run([sys.executable, str(ROOT / "scripts/fixture-tools.py"), tool, str(ROOT)],
                           cwd=ROOT, env=environment, check=True)
        with storage.workspace("fixture-feed") as directory:
            subprocess.run([sys.executable, str(ROOT / "scripts/fixture-packages.py"),
                            str(ROOT), str(directory)], cwd=ROOT, env=environment, check=True)


if __name__ == "__main__":
    main()
