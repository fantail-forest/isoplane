#!/usr/bin/env python3
"""Build a Windows SDK MSI from an assembled toolchain: --toolchain DIR --version VERSION --output FILE.msi."""

import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import subprocess
import shutil
import tempfile
import uuid
import zipfile
import xml.etree.ElementTree as ET

NAMESPACE = 'http://schemas.microsoft.com/wix/2006/wi'
IDENTITY = uuid.UUID('9fae432c-ad48-44b7-81b0-12fecf1e3ee1')


def identifier(path):
    return 'i' + hashlib.sha256(path.encode()).hexdigest()[:32]


def guid(name):
    return str(uuid.uuid5(IDENTITY, name)).upper()


def properties():
    project = ET.Element('Project')
    group = ET.SubElement(project, 'PropertyGroup')
    for name, relative in [('IsoplaneGuestRuntimeFeed', '../feed'), ('WASI_SDK_PATH', '../wasi-sdk'),
                           ('IsoplaneCliAssembly', '../cli/Isoplane.Cli.dll'),
                           ('IsoplaneMonoTargetsDirectory', '../mono-targets/Sdk'),
                           ('IsoplaneWasiCrossDirectory', '../wasi-cross/Sdk')]:
        ET.SubElement(group, name, Condition=f"'$({name})' == ''").text = (
            f"$([MSBuild]::NormalizeDirectory('$(MSBuildThisFileDirectory)', '{relative}'))"
            if name.endswith('Directory') else
            f"$([MSBuild]::NormalizePath('$(MSBuildThisFileDirectory)', '{relative}'))")
    return ET.tostring(project, encoding='utf-8', xml_declaration=True)


def author(toolchain, version, output):
    if not re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.-]+)?', version):
        raise ValueError('version must be a three-part NuGet version, optionally with a prerelease suffix')
    manifest = json.loads((toolchain / 'toolchain.json').read_text())
    if manifest['runtimeIdentifier'] != 'win-x64':
        raise ValueError('the Windows MSI requires a win-x64 toolchain')
    paths = []
    for entry in manifest['files']:
        relative = entry['path']
        if any(part in ('', '.', '..') for part in relative.split('/')) or '\\' in relative or ':' in relative:
            raise ValueError(f'unsafe toolchain path: {relative}')
        if relative.split('/')[0].lower() in ('dotnet', 'sdk-manifests', 'metadata'):
            raise ValueError(f'the MSI must not install .NET SDK or workload state: {relative}')
        path = toolchain / relative
        if path.is_symlink():
            raise ValueError(f'the Windows payload must contain regular files: {relative}')
        with path.open('rb') as source:
            if hashlib.file_digest(source, 'sha256').hexdigest() != entry['sha256']:
                raise ValueError(f'toolchain integrity check failed: {relative}')
        paths.append(relative)
    required = {'Sdk/Sdk.props', 'feed/Isoplane.GuestRuntime.props', 'compiler/Isoplane.GuestCompiler.props',
                'cli/Isoplane.Cli.dll', 'cli/runtime/isoplane_native.dll', 'bin/wasm-tools.exe',
                'wasi-sdk/bin/clang.exe', 'mono-targets/Sdk/Sdk.props', 'wasi-cross/Sdk/Sdk.props',
                f'templates/Isoplane.Templates.{version}.nupkg'}
    if not required.issubset(paths):
        raise ValueError(f'toolchain is missing: {sorted(required - set(paths))}')
    if len(paths) != len(set(path.casefold() for path in paths)):
        raise ValueError('toolchain paths collide on Windows')
    ET.register_namespace('', NAMESPACE)
    def element(parent, name, **attributes):
        return ET.SubElement(parent, '{' + NAMESPACE + '}' + name, attributes)
    root = ET.Element('{' + NAMESPACE + '}Wix')
    product = element(root, 'Product', Id=guid('product/' + version), Name='Isoplane SDK ' + version,
                      Language='1033', Version=version.split('-')[0], Manufacturer='Isoplane',
                      UpgradeCode=guid('upgrade/' + version))
    element(product, 'Package', InstallerVersion='500', Compressed='yes', InstallScope='perMachine')
    element(product, 'Media', Id='1', Cabinet='sdk.cab', EmbedCab='yes')
    target = element(product, 'Directory', Id='TARGETDIR', Name='SourceDir')
    directory = element(target, 'Directory', Id='ProgramFiles64Folder')
    for name in ['Isoplane', 'SDK', version]:
        directory = element(directory, 'Directory', Id=identifier('directory/' + name), Name=name)
    directories = {'': directory}
    feature = element(product, 'Feature', Id='Sdk', Title='Isoplane SDK', Level='1')
    for relative in sorted([*paths, 'toolchain.json']):
        parent = Path(relative).parent.as_posix()
        if parent == '.':
            parent = ''
        parts = parent.split('/') if parent else []
        current = ''
        for part in parts:
            child = '/'.join(filter(None, [current, part]))
            if child not in directories:
                directories[child] = element(directories[current], 'Directory', Id=identifier('payload/' + child), Name=part)
            current = child
        component_id = identifier('component/' + relative)
        component = element(directories[parent], 'Component', Id=component_id, Guid=guid(version + '/' + relative), Win64='yes')
        element(component, 'File', Id=identifier('file/' + relative), Name=Path(relative).name,
                Source=str((toolchain / relative).resolve()), KeyPath='yes', DiskId='1')
        element(feature, 'ComponentRef', Id=component_id)
    ET.ElementTree(root).write(output, encoding='utf-8', xml_declaration=True)



def write_release(toolchain, bundle, packages, version, revision, destination):
    root = Path(__file__).resolve().parent.parent
    spec = importlib.util.spec_from_file_location('build_preview', root / 'scripts/build-preview.py')
    preview = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(preview)
    preview.verify_packages(packages, version, revision, toolchain / 'cli/runtime/isoplane_native.dll', 'win-x64')
    template = f'Isoplane.Templates.{version}.nupkg'
    if preview.sha(packages / template) != preview.sha(toolchain / 'templates' / template):
        raise ValueError('MSI template differs from the release package')
    with zipfile.ZipFile(packages / f'Isoplane.Cli.{version}.nupkg') as archive:
        prefix = 'tools/net10.0/any/'
        for entry in archive.infolist():
            if entry.filename.startswith(prefix) and not entry.is_dir():
                relative = entry.filename[len(prefix):]
                if any(part in ('', '.', '..') for part in relative.split('/')) or '\\' in relative or ':' in relative:
                    raise ValueError(f'unsafe CLI package path: {relative}')
                with archive.open(entry) as source:
                    if hashlib.file_digest(source, 'sha256').hexdigest() != preview.sha(toolchain / 'cli' / relative):
                        raise ValueError(f'MSI CLI differs from the release package: {relative}')
    if destination.exists():
        raise ValueError('release output must not already exist')
    epoch = int(subprocess.check_output(['git', 'show', '-s', '--format=%ct', revision], cwd=root, text=True))
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.windows-release-', dir=destination.parent) as temporary:
        stage = Path(temporary) / 'release'
        stage.mkdir()
        shutil.copytree(bundle, stage / 'toolchain')
        shutil.copytree(packages, stage / 'packages')
        manifest = {'sourceRevision': revision, 'packageVersion': version, 'runtimeIdentifier': 'win-x64',
                    'toolchain': preview.archive_toolchain(stage, epoch),
                    'packages': [{'id': name, 'version': version, 'file': f'packages/{name}.{version}.nupkg',
                                  'sha256': preview.sha(stage / 'packages' / f'{name}.{version}.nupkg')}
                                 for name in preview.PROJECTS]}
        (stage / 'release.json').write_text(json.dumps(manifest, indent=2) + '\n')
        stage.rename(destination)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--toolchain', type=Path, required=True)
    parser.add_argument('--version', required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--bundle', type=Path, help='write the setup bundle containing this MSI')
    parser.add_argument('--vc-redist', type=Path, help='Microsoft VC++ x64 redistributable to include in the setup bundle')
    parser.add_argument('--release', type=Path, help='write a complete setup release from the bundle and six NuGet packages')
    parser.add_argument('--packages', type=Path)
    parser.add_argument('--revision', help='source commit shared by all six NuGet packages')
    args = parser.parse_args()
    if args.bundle and not args.vc_redist:
        parser.error('--bundle requires --vc-redist')
    if args.release and not (args.bundle and args.packages and args.revision):
        parser.error('--release requires --bundle, --packages and --revision')
    if args.output.exists() or (args.bundle and args.bundle.exists()) or (args.release and args.release.exists()):
        parser.error('output paths must not already exist; use a new release version or destination')
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='isoplane-msi-') as temporary:
        authoring = Path(temporary) / 'sdk.wxs'
        author(args.toolchain, args.version, authoring)
        subprocess.run(['wixl', '-a', 'x64', '-o', str(args.output.resolve()), str(authoring)], check=True)
    if args.bundle:
        args.bundle.mkdir(parents=True, exist_ok=False)
        shutil.copy2(args.output, args.bundle / 'sdk.msi')
        shutil.copy2(args.vc_redist, args.bundle / 'vc_redist.x64.exe')
        (args.bundle / 'WindowsInstaller.json').write_text(json.dumps({'Version': args.version, 'ProductCode': guid('product/' + args.version)}))
        manifest = json.loads((args.toolchain / 'toolchain.json').read_text())
        manifest.update(kind='msi', files=[])
        for path in sorted(args.bundle.iterdir()):
            with path.open('rb') as stream:
                manifest['files'].append({'path': path.name, 'sha256': hashlib.file_digest(stream, 'sha256').hexdigest(), 'executable': False})
        (args.bundle / 'toolchain.json').write_text(json.dumps(manifest, indent=2) + '\n')

    if args.release:
        write_release(args.toolchain, args.bundle, args.packages, args.version, args.revision, args.release)


if __name__ == '__main__':
    main()
