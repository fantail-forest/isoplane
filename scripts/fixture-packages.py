#!/usr/bin/env python3
"""Build the installed SDK/CLI fixture packages once per source and toolchain identity."""

import importlib.util
import json
import os
from pathlib import Path
import subprocess
import shutil
import sys
import xml.etree.ElementTree as ET


def load(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


modules = load('shared-runtime-modules')
storage = load('build_storage')
managed = load('managed_packages')

def shared_runtime(root):
    """Obtain the Linux-built runtime, or verify explicitly selected portable artifacts."""
    selected = os.environ.get('ISOPLANE_TEST_RUNTIME_MODULES')
    if selected:
        directory = Path(selected)
        if not directory.is_absolute():
            raise ValueError('ISOPLANE_TEST_RUNTIME_MODULES must name an absolute artifact directory')
        feed = Path(os.environ.get('ISOPLANE_TEST_RUNTIME_FEED', root / '.build/wasm32/runtime/feed'))
        version = ET.parse(root / 'Directory.Build.props').findtext('.//Version')
        revision = json.loads((directory / 'runtime.json').read_text())['sourceRevision']
        modules.verify(directory, version, revision, feed)
        print(f'explicitly selected shared runtime artifacts: {directory} ({revision})', file=sys.stderr)
        return directory
    subprocess.run([sys.executable, str(root / "scripts/dev"), "bash", str(root / "guest-runtime/tests/packages/build.sh"), "--if-stale", "--fixture", "runtime"],
                   cwd=root, check=True, stdout=sys.stderr)
    return root / ".build/wasm32/runtime-modules"


def package_feed(root, destination):
    import tasks
    root = Path(root).resolve()
    shared_runtime(root)
    tasks.run(root, "fixtures:feed")
    return copy_feed(storage.build_root(root) / "fixture-packages/packages", destination)


def pack_managed(root):
    if sys.platform != "linux":
        raise RuntimeError("Shared fixture packages are missing or stale; build them on Linux first.")
    runtime = shared_runtime(root)
    compiled = root / ".build/clr-anycpu/fixture-packages"
    projects = (
        "guest-sdks/dotnet/Isoplane.Sdk.csproj",
        "guest-sdks/dotnet/AspNetCore/Isoplane.AspNetCore.csproj",
        "guest-sdks/dotnet/Templates/Isoplane.Templates.csproj",
        "tools/Isoplane.Cli/Isoplane.Cli.csproj",
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
        managed.attach_native(package, 'Isoplane.Cli' if package.name.startswith('Isoplane.Cli.') else '',
                              native, storage.platform_name())


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
        print(package_feed(sys.argv[1], sys.argv[2]))
