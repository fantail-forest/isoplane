#!/usr/bin/env python3
"""Build the installed SDK/CLI fixture packages once per source and toolchain identity."""

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import xml.etree.ElementTree as ET


def load(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


tools = load('fixture-tools')
modules = load('shared-runtime-modules')

def shared_runtime(root):
    """Build under the C# fixture lock, or verify explicitly selected portable artifacts."""
    selected = os.environ.get('ISOPLANE_TEST_RUNTIME_MODULES')
    if selected:
        directory = Path(selected)
        if not directory.is_absolute():
            raise ValueError('ISOPLANE_TEST_RUNTIME_MODULES must name an absolute artifact directory')
        feed = Path(os.environ.get('ISOPLANE_TEST_RUNTIME_FEED', root / 'guest-runtime/out/feed'))
        version = ET.parse(root / 'Directory.Build.props').findtext('.//Version')
        revision = json.loads((directory / 'runtime-build.json').read_text())['sourceRevision']
        modules.verify(directory, version, revision, feed)
        print(f'explicitly selected shared runtime artifacts: {directory} ({revision})', file=sys.stderr)
        return directory
    subprocess.run(["bash", str(root / "guest-runtime/tests/packages/build.sh"), "--if-stale", "--fixture", "runtime"],
                   cwd=root, check=True, stdout=sys.stderr)
    return root / "guest-runtime/tests/packages/out/runtime"


def package_feed(root):
    root = Path(root).resolve()
    runtime = shared_runtime(root)
    sources = [root / name for name in (
        "guest-sdks/dotnet", "tools", "crates", ".cargo", "Cargo.toml",
        "Cargo.lock", "Directory.Build.props", "global.json", "nuget.config",
        "scripts/fixture-tools.py", "scripts/fixture-packages.py",
    )] + modules.files(runtime)
    toolchains = subprocess.check_output(["dotnet", "--version"], cwd=root)
    toolchains += subprocess.check_output(["rustc", "-vV"], cwd=root)
    identity = hashlib.sha256(tools.fingerprint(sources).encode() + toolchains).hexdigest()
    target = root / "target" / "fixture-packages" / identity
    target.mkdir(parents=True, exist_ok=True)
    packages = target / "packages"
    receipt = target / "packages.json"
    with (target / "build.lock").open("a+b") as handle:
        tools.lock_file(handle)
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
                     str(target / "artifacts"), "-o", str(packages), "--disable-build-servers",
                     f"-p:IsoplaneRuntimeModuleDirectory={runtime}"],
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
