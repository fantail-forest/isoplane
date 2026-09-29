#!/usr/bin/env python3
"""Package SDK/CLI fixtures using the prepared shared runtime."""

import importlib.util
import os
from pathlib import Path
import subprocess
import shutil
import sys


def load(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


storage = load('build_storage')
managed = load('managed_packages')

def pack_managed(root):
    if sys.platform != "linux":
        raise RuntimeError("Shared fixture packages are missing or stale; build them on Linux first.")
    runtime = Path(os.environ.get("ISOPLANE_TEST_RUNTIME_MODULES", root / ".build/wasm32/runtime-modules"))
    compiled = root / ".build/clr-anycpu/fixture-packages"
    projects = (
        "guest-sdks/dotnet/Isoplane.Sdk.csproj",
        "guest-sdks/dotnet/AspNetCore/Isoplane.AspNetCore.csproj",
        "guest-sdks/dotnet/Templates/Isoplane.Templates.csproj",
        "tools/Isoplane.Cli/Isoplane.Cli.csproj",
        "tools/Isoplane.Native/Isoplane.Native.csproj",
        "tools/Isoplane.Testing/Isoplane.Testing.csproj",
    )
    for project in projects:
        subprocess.run(['dotnet', 'pack', str(root / project), '--artifacts-path',
                        str(compiled / 'artifacts'), '-o', str(compiled / 'packages'),
                        '--disable-build-servers', '-p:IsoplaneManagedOnly=true', '-p:UseAppHost=false',
                        f'-p:IsoplaneRuntimeModuleDirectory={runtime}'], cwd=root, check=True)


def attach_native(root):
    packages = storage.build_root(root) / "fixture-packages/packages"
    source = root / ".build/clr-anycpu/fixture-packages/packages"
    copy_feed(source, packages)
    library = {"win32": "isoplane_native.dll", "darwin": "libisoplane_native.dylib"}.get(sys.platform, "libisoplane_native.so")
    native = storage.build_root(root) / "native/release" / library
    for package in packages.glob("*.nupkg"):
        name = next((name for name in ('Isoplane.Cli', 'Isoplane.Native')
                     if package.name.startswith(name + '.')), '')
        managed.attach_native(package, name, native, storage.platform_name())


def copy_feed(packages, destination):
    destination = Path(destination)
    destination.mkdir(parents=True, exist_ok=True)
    for package in packages.glob("*.nupkg"):
        shutil.copy2(package, destination / package.name)
    return destination


if __name__ == "__main__":
    root = Path(__file__).resolve().parents[1]
    if sys.argv[1] == "pack-managed":
        pack_managed(root)
    elif sys.argv[1] == "attach-native":
        attach_native(root)
    else:
        sys.exit("expected pack-managed or attach-native")
