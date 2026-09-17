#!/usr/bin/env python3
"""Assemble an integrity-checked local preview toolchain from built artifacts."""
import argparse
import hashlib
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import tempfile
import xml.etree.ElementTree as ET
import zipfile


def verify_native_pair(package_path, library):
    with zipfile.ZipFile(package_path) as package:
        entries = [entry for entry in package.namelist() if entry.endswith('/runtime/' + library.name)]
        if len(entries) != 1:
            raise ValueError(f'CLI package must contain exactly one runtime/{library.name}')
        with package.open(entries[0]) as bundled, library.open('rb') as selected:
            if hashlib.file_digest(bundled, 'sha256').digest() != hashlib.file_digest(selected, 'sha256').digest():
                raise ValueError('CLI package and selected native library differ; rebuild the CLI package')


def extract_cli(package_path, destination):
    prefix = 'tools/net10.0/any/'
    with zipfile.ZipFile(package_path) as package:
        for entry in package.infolist():
            if not entry.filename.startswith(prefix) or entry.is_dir():
                continue
            relative = entry.filename[len(prefix):]
            if not relative or any(part in ('', '.', '..') for part in relative.split('/')) or '\\' in relative:
                raise ValueError(f'Unsafe CLI package path: {entry.filename}')
            output = destination / relative
            output.parent.mkdir(parents=True, exist_ok=True)
            with package.open(entry) as source, output.open('wb') as target:
                shutil.copyfileobj(source, target)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--runtime-output', type=Path, required=True, help='guest-runtime/out containing feed and packs')
    parser.add_argument('--wasi-sdk', type=Path, required=True)
    parser.add_argument('--wasm-tools', type=Path, required=True)
    parser.add_argument('--native-library', type=Path, required=True, help='built isoplane-native library matching the CLI package')
    parser.add_argument('--cli-package', type=Path, help='CLI package for a toolchain-only bundle')
    parser.add_argument('--dotnet-root', type=Path, required=True)
    parser.add_argument('--notices', type=Path, required=True, help='output from toolchain-notices.py')
    parser.add_argument('--packages', type=Path, help='include this SDK/CLI/template NuGet feed in a complete preview release')
    parser.add_argument('--package-version', help='one version shared by the CLI, SDK, templates and testing package')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    if args.packages and not args.package_version:
        parser.error('--packages requires --package-version')
    if not args.packages and args.package_version:
        parser.error('--package-version requires --packages')
    if args.packages:
        for name in ['Isoplane.Cli', 'Isoplane.Native', 'Isoplane.Testing', 'Wisp.Sdk', 'Wisp.AspNetCore', 'Wisp.Templates']:
            if not (args.packages / f'{name}.{args.package_version}.nupkg').is_file():
                parser.error(f'local feed is missing {name} {args.package_version}')
        args.cli_package = args.packages / f'Isoplane.Cli.{args.package_version}.nupkg'
    if not args.cli_package:
        parser.error('supply --cli-package or a complete --packages feed')
    try:
        verify_native_pair(args.cli_package, args.native_library)
    except ValueError as error:
        parser.error(str(error))
    sdk = json.loads((root / 'global.json').read_text())['sdk']['version']
    runtime_version = re.search(r'^RUNTIME_TAG=v(.+)$', (root / 'guest-runtime/manifest.env').read_text(), re.MULTILINE).group(1)
    expected_wasi = re.search(r'^WASI_SDK_VERSION=(.+)$', (root / 'guest-runtime/manifest.env').read_text(), re.MULTILINE).group(1)
    if (args.wasi_sdk / 'VERSION').read_text().splitlines()[0] != expected_wasi:
        parser.error(f'wasi-sdk must match the runtime build: {expected_wasi}')
    expected_wasm = re.search(r'cargo install --locked wasm-tools@([^\s]+)', (root / 'Dockerfile').read_text()).group(1)
    actual_wasm = subprocess.check_output([str(args.wasm_tools), '--version'], text=True).strip()
    if actual_wasm != f'wasm-tools {expected_wasm}':
        parser.error(f'expected wasm-tools {expected_wasm}; got {actual_wasm}')
    if not (args.dotnet_root / 'sdk' / sdk).is_dir():
        parser.error(f'dotnet root does not contain pinned SDK {sdk}')
    subprocess.run(['python3', str(root / 'guest-runtime/feed-identity.py'), 'verify-current', str(root / 'guest-runtime'), str(args.runtime_output / 'feed')], check=True, stdout=subprocess.DEVNULL)
    subprocess.run(['python3', str(root / 'guest-runtime/compiler/build.py'), 'verify-current', '--output', str(args.runtime_output / 'compiler')], check=True, stdout=subprocess.DEVNULL)
    if subprocess.check_output([str(args.dotnet_root / 'dotnet'), '--list-sdks'], text=True).splitlines() == []:
        parser.error('dotnet root is not runnable')
    if not (args.notices / 'rust-dependencies.json').is_file():
        parser.error('notices collection is incomplete; run toolchain-notices.py first')
    if args.output.exists():
        parser.error('output must not already exist')
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.toolchain-', dir=args.output.parent) as temporary:
        release = Path(temporary) / 'release'
        stage = release / 'toolchain' if args.packages else release
        stage.mkdir(parents=True)
        for source, name in [(args.runtime_output / 'feed', 'feed'), (args.runtime_output / 'packs', 'packs'), (args.runtime_output / 'compiler', 'compiler'),
                             (args.wasi_sdk, 'wasi-sdk'), (args.dotnet_root, 'dotnet'), (args.notices, 'notices')]:
            shutil.copytree(source, stage / name, symlinks=False)
        environment = os.environ.copy()
        environment.update(DOTNET_CLI_HOME=str(stage / '.setup-home'), DOTNET_CLI_TELEMETRY_OPTOUT='1', DOTNET_NOLOGO='1')
        subprocess.run([str((stage / 'dotnet/dotnet').resolve()), 'workload', 'install', 'wasi-experimental',
            '--skip-manifest-update', '--source', 'https://api.nuget.org/v3/index.json'], cwd=stage,
            env=environment, check=True)
        shutil.rmtree(stage / '.setup-home', ignore_errors=False)
        # SDK first use recreates these records with absolute local pack paths.
        # They are installation state, not reproducible toolchain payload.
        shutil.rmtree(stage / 'dotnet/metadata/workloads/InstalledPacks')
        for pack in ('Microsoft.NET.Runtime.MonoTargets.Sdk', 'Microsoft.NETCore.App.Runtime.AOT.linux-x64.Cross.wasi-wasm'):
            if not (stage / 'dotnet/packs' / pack / runtime_version / 'Sdk/Sdk.props').is_file():
                parser.error(f'pinned SDK did not provide expected WASI build pack {pack} {runtime_version}')
        (stage / 'bin').mkdir()
        shutil.copy2(args.wasm_tools, stage / 'bin/wasm-tools')
        extract_cli(args.cli_package, stage / 'cli')
        entries = []
        for path in sorted(stage.rglob('*')):
            if path.is_file():
                with path.open('rb') as stream:
                    digest = hashlib.file_digest(stream, 'sha256').hexdigest()
                entries.append(dict(path=path.relative_to(stage).as_posix(), sha256=digest, executable=bool(path.stat().st_mode & 0o100)))
        manifest = dict(runtimeIdentifier='linux-x64', dotnetSdkVersion=sdk, wasiWorkloadVersion=runtime_version, files=entries)
        (stage / 'toolchain.json').write_text(json.dumps(manifest, indent=2) + '\n')
        if args.packages:
            shutil.copytree(args.packages, release / 'packages')
            readme = (root / 'scripts/developer-preview-readme.md').read_text()
            readme = readme.replace('@PACKAGE_VERSION@', args.package_version)
            (release / 'package-version.txt').write_text(args.package_version + '\n')
            (release / 'README.md').write_text(readme)
            sample = release / 'samples/aspnet-todo'
            shutil.copytree(root / 'samples/aspnet-todo', sample,
                            ignore=shutil.ignore_patterns('bin', 'obj', 'out', '.wisp', '.wisp-dev', '.local-postgres.json'))
            project = sample / 'Todo.csproj'
            document = ET.parse(project)
            if document.getroot().get('Sdk', '').split('/')[0] != 'Wisp.AspNetCore':
                parser.error('the database sample must use the Isoplane ASP.NET SDK')
            document.getroot().set('Sdk', 'Wisp.AspNetCore/' + args.package_version)
            document.write(project, encoding='unicode')

        release.rename(args.output)
    print(args.output)


if __name__ == '__main__':
    main()
