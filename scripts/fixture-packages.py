#!/usr/bin/env python3
"""Build the installed SDK/CLI fixture packages once per source and toolchain identity."""

import hashlib
import importlib.util
import json
import os
import platform
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


tools = load('fixture-tools')
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
        feed = Path(os.environ.get('ISOPLANE_TEST_RUNTIME_FEED', root / 'guest-runtime/out/feed'))
        version = ET.parse(root / 'Directory.Build.props').findtext('.//Version')
        revision = json.loads((directory / 'runtime-build.json').read_text())['sourceRevision']
        modules.verify(directory, version, revision, feed)
        print(f'explicitly selected shared runtime artifacts: {directory} ({revision})', file=sys.stderr)
        return directory
    subprocess.run([sys.executable, str(root / "scripts/dev"), "bash", str(root / "guest-runtime/tests/packages/build.sh"), "--if-stale", "--fixture", "runtime"],
                   cwd=root, check=True, stdout=sys.stderr)
    return root / ".build/wasm32/runtime-modules"


def package_feed(root, destination):
    root = Path(root).resolve()
    runtime = shared_runtime(root)
    sources = [root / name for name in (
        "guest-sdks/dotnet", "tools", "Directory.Build.props", "global.json", "nuget.config",
        "scripts/fixture-tools.py", "scripts/fixture-packages.py", "scripts/managed_packages.py",
    )] + modules.files(runtime)
    toolchains = subprocess.check_output(["dotnet", "--version"], cwd=root).strip()
    portable_identity = hashlib.sha256(tools.fingerprint(sources).encode() + toolchains).hexdigest()
    toolchains += subprocess.check_output(["rustc", "-vV"], cwd=root)
    native_sources = [root / name for name in ("crates", ".cargo", "Cargo.toml", "Cargo.lock")]
    identity = hashlib.sha256(portable_identity.encode() + tools.fingerprint(native_sources).encode() + toolchains).hexdigest()
    with storage.workspace(f"fixture-packages:{sys.platform}:{platform.machine()}") as target:
        return build_feed(root, destination, runtime, identity, target, portable_identity)


def build_feed(root, destination, runtime, identity, target, portable_identity):
    target.mkdir(parents=True, exist_ok=True)
    packages = target / "packages"
    receipt = target / "packages.json"
    outputs = sorted(packages.glob("*.nupkg"))
    if outputs and tools.is_fresh(receipt, identity, outputs):
        return copy_feed(packages, destination)
    if packages.exists():
        shutil.rmtree(packages)
    # A different checkout can have older timestamps but different source bytes.
    with storage.product("fixture-packages", root=root / ".build/clr-anycpu", name="fixture-packages") as compiled:
        intermediates = compiled / "artifacts/obj"
        receipt_identity = compiled / "inputs"
        if sys.platform == "linux" and (not receipt_identity.exists() or receipt_identity.read_text() != portable_identity):
            if intermediates.exists():
                shutil.rmtree(intermediates)
            receipt_identity.write_text(portable_identity)
    native = Path(subprocess.check_output(
        [sys.executable, str(root / "scripts/fixture-tools.py"), "isoplane-native", str(root), "release"],
        cwd=root, text=True).strip())
    projects = (
        "guest-sdks/dotnet/Isoplane.Sdk.csproj",
        "guest-sdks/dotnet/AspNetCore/Isoplane.AspNetCore.csproj",
        "guest-sdks/dotnet/Templates/Isoplane.Templates.csproj",
        "tools/Isoplane.Cli/Isoplane.Cli.csproj",
    )
    log = target / "build.log"
    version = ET.parse(root / 'Directory.Build.props').findtext('.//Version')
    environment = storage.temporary_environment(target / 'tmp', os.environ)
    with log.open('w') as output:
        for project in projects:
            def pack():
                result = subprocess.run(
                    ['dotnet', 'pack', str(root / project), '--artifacts-path',
                     str(compiled / 'artifacts'), '-o', str(packages), '--disable-build-servers', '-p:IsoplaneManagedOnly=true', '-p:UseAppHost=false',
                     f'-p:IsoplaneRuntimeModuleDirectory={runtime}'],
                    cwd=root, env=environment, stdout=output, stderr=subprocess.STDOUT)
                if result.returncode != 0:
                    raise RuntimeError(f'packing {project} failed; see {log}')
            name = Path(project).stem
            package = packages / f'{name}.{version}.nupkg'
            storage.cached_files(f'fixture-managed-package:{portable_identity}:{name}',
                                 [package], pack, linux_only=True, target="clr-anycpu")
            managed.attach_native(package, name, native, storage.platform_name())
    outputs = sorted(packages.glob("*.nupkg"))
    if not outputs:
        raise RuntimeError(f"package build produced no packages; see {log}")
    tools.write_receipt(receipt, identity, outputs)
    return copy_feed(packages, destination)

def copy_feed(packages, destination):
    destination = Path(destination)
    destination.mkdir(parents=True, exist_ok=True)
    for package in packages.glob("*.nupkg"):
        shutil.copy2(package, destination / package.name)
    return destination


if __name__ == "__main__":
    print(package_feed(sys.argv[1], sys.argv[2]))
