#!/usr/bin/env python3
"""Build the installed SDK/CLI fixture packages once per source and toolchain identity."""

import fcntl
import hashlib
import importlib.util
from pathlib import Path
import subprocess
import sys

SPEC = importlib.util.spec_from_file_location("fixture_tools", Path(__file__).with_name("fixture-tools.py"))
tools = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(tools)

def package_feed(root):
    root = Path(root).resolve()
    sources = [root / name for name in (
        "guest-sdks/dotnet", "tools", "crates", ".cargo", "Cargo.toml",
        "Cargo.lock", "Directory.Build.props", "global.json", "nuget.config",
        "scripts/fixture-tools.py", "scripts/fixture-packages.py",
    )]
    toolchains = subprocess.check_output(["dotnet", "--version"], cwd=root)
    toolchains += subprocess.check_output(["rustc", "-vV"], cwd=root)
    identity = hashlib.sha256(tools.fingerprint(sources).encode() + toolchains).hexdigest()
    target = root / "target" / "fixture-packages" / identity
    target.mkdir(parents=True, exist_ok=True)
    packages = target / "packages"
    receipt = target / "packages.json"
    with (target / "build.lock").open("a") as handle:
        fcntl.flock(handle, fcntl.LOCK_EX)
        outputs = sorted(packages.glob("*.nupkg"))
        if outputs and tools.is_fresh(receipt, identity, outputs):
            return packages
        projects = (
            "guest-sdks/dotnet/Isoplane.Sdk.csproj",
            "guest-sdks/dotnet/AspNetCore/Isoplane.AspNetCore.csproj",
            "guest-sdks/dotnet/Templates/Isoplane.Templates.csproj",
            "tools/Isoplane.Cli/Isoplane.Cli.csproj",
        )
        log = target / "build.log"
        with log.open("w") as output:
            for project in projects:
                result = subprocess.run(
                    ["dotnet", "pack", str(root / project), "--artifacts-path",
                     str(target / "artifacts"), "-o", str(packages), "--disable-build-servers"],
                    cwd=root, stdout=output, stderr=subprocess.STDOUT,
                )
                if result.returncode != 0:
                    raise RuntimeError(f"packing {project} failed; see {log}")
        outputs = sorted(packages.glob("*.nupkg"))
        if not outputs:
            raise RuntimeError(f"package build produced no packages; see {log}")
        tools.write_receipt(receipt, identity, outputs)
    return packages


if __name__ == "__main__":
    print(package_feed(sys.argv[1]))
