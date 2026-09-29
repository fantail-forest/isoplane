#!/usr/bin/env python3
"""Build a Windows x64 SDK release using the Linux-built shared runtime."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import tarfile
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parents[1]


def load(name):
    if name == "build_storage" and name in sys.modules:
        return sys.modules[name]
    path = ROOT / 'scripts' / (name + '.py')
    spec = importlib.util.spec_from_file_location(name.replace('-', '_'), path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    if name == "build_storage":
        sys.modules[name] = module
    return module


def developer_environment(msys):
    locator = Path(os.environ.get('ProgramFiles(x86)', 'C:/Program Files (x86)')) / 'Microsoft Visual Studio/Installer/vswhere.exe'
    installation = subprocess.check_output([str(locator), '-latest', '-products', '*',
        '-requires', *json.loads((ROOT / 'scripts/windows-build-components.json').read_text()), '-property', 'installationPath'], text=True).strip()
    if not installation:
        raise ValueError('Install Visual Studio C++ Build Tools, including ATL and CMake tools.')
    command = f'call "{installation}\\Common7\\Tools\\VsDevCmd.bat" -arch=amd64 -host_arch=amd64 >nul && set'
    result = subprocess.check_output('cmd.exe /u /d /s /c "' + command + '"', encoding='utf-16-le')
    environment = dict(os.environ)
    for line in result.splitlines():
        key, separator, value = line.partition('=')
        if separator and key:
            environment[key.upper()] = value
    environment['PATH'] += os.pathsep + str(msys / 'mingw64/bin')
    environment.update(MSYSTEM='MINGW64', MSYS2_PATH_TYPE='inherit', CHERE_INVOKING='1')
    index = int(environment.get('GIT_CONFIG_COUNT', '0'))
    environment.update({f'GIT_CONFIG_KEY_{index}': 'core.longpaths',
                        f'GIT_CONFIG_VALUE_{index}': 'true', 'GIT_CONFIG_COUNT': str(index + 1)})
    return environment, Path(installation)


def redistributable(installation, requested):
    if requested is None:
        candidates = sorted((installation / 'VC/Redist/MSVC').glob('*/vc_redist.x64.exe'))
        if not candidates:
            raise ValueError('Visual C++ redistributable not found; supply --vc-redist')
        requested = candidates[-1]
    if not requested.is_file():
        raise ValueError(f'Visual C++ redistributable not found: {requested}')
    return requested.resolve()


def runtime_command(msys, runtime, stages):
    return [msys / 'usr/bin/bash.exe', '-lc', 'exec bash "$@"', 'isoplane-runtime',
            (runtime / 'build.sh').as_posix(), *stages]


def stock_pack(name, version, cache):
    destination = cache / name / version
    archive = cache / f'{name}.{version}.nupkg'
    if destination.exists():
        return destination
    url = f'https://api.nuget.org/v3-flatcontainer/{name.lower()}/{version}/{name.lower()}.{version}.nupkg'
    with urllib.request.urlopen(url, timeout=60) as response, archive.open('wb') as stream:
        shutil.copyfileobj(response, stream)
    with zipfile.ZipFile(archive) as package:
        package.extractall(destination)
    return destination



def build(args):
    preview = load('build-preview')
    revision = preview.source_revision(ROOT)
    epoch = int(preview.output(['git', 'show', '-s', '--format=%ct', revision]))
    version = preview.release_version(ROOT)
    if args.runtime_modules:
        load('shared-runtime-modules').verify(args.runtime_modules.resolve(), version, revision)
    cache = args.cache.resolve()
    logs = cache / 'logs'
    logs.mkdir(parents=True, exist_ok=True)
    releases = args.output.resolve()
    releases.mkdir(parents=True, exist_ok=True)
    release = releases / f'isoplane-win-x64-{version}'
    archive = release.with_name(release.name + '.tgz')
    if preview.existing_release(release, archive, revision):
        print(f'Verified existing release: {release}')
        return
    source = ROOT
    runtime = source / 'guest-runtime'
    environment, visual_studio = developer_environment(args.msys_root.resolve())
    redist = redistributable(visual_studio, args.vc_redist)
    sdk, _ = preview.prepare_sdk(source, cache, logs, 'win-x64')
    native_target = load('build_storage').build_root(source) / 'native'
    environment.update(DOTNET_ROOT=str(sdk), DOTNET_CLI_HOME=str(cache / 'dotnet-home'),
        DOTNET_CLI_TELEMETRY_OPTOUT='1', DOTNET_NOLOGO='1',
        CARGO_TARGET_DIR=str(native_target), CARGO_BUILD_BUILD_DIR=str(native_target))
    environment['PATH'] = str(sdk) + os.pathsep + environment['PATH']

    def run(command, name):
        preview.run(command, source, logs / (name + '.log'), environment)

    runtime_output = ROOT / '.build/wasm32/runtime'
    runtime_work = load('build_storage').build_root(ROOT) / 'development/runtime-work'
    run(runtime_command(args.msys_root.resolve(), runtime, ['toolchain']), 'runtime-tools')
    run(['cargo', 'build', '--locked', '--release', '-p', 'isoplane-native'], 'native')
    native = native_target / 'release/isoplane_native.dll'
    wasm_version = re.search(r'cargo install --locked wasm-tools@([^\s]+)', (source / 'Dockerfile').read_text()).group(1)
    run(['cargo', 'install', '--locked', f'wasm-tools@{wasm_version}', '--root', cache / 'tools'], 'wasm-tools')
    runtime_modules = args.runtime_modules.resolve() if args.runtime_modules else ROOT / '.build/wasm32/release-runtime'
    load('shared-runtime-modules').verify(runtime_modules, version, revision, runtime_output / 'feed')
    packages = cache / 'packages'
    if packages.exists():
        shutil.rmtree(packages)
    packages.mkdir()
    for name in preview.PROJECTS:
        preview.pack_project(name, version, packages, native, 'win-x64')
    preview.verify_packages(packages, version, revision, native, 'win-x64')
    runtime_version = re.search(r'^RUNTIME_TAG=v(.+)$', (runtime / 'manifest.env').read_text(), re.MULTILINE).group(1)
    mono = stock_pack('Microsoft.NET.Runtime.MonoTargets.Sdk', runtime_version, cache)
    cross = stock_pack('Microsoft.NETCore.App.Runtime.AOT.win-x64.Cross.wasi-wasm', runtime_version, cache)
    cargo_home = Path(os.environ.get('CARGO_HOME', Path.home() / '.cargo'))
    wasm_sources = list((cargo_home / 'registry/src').glob(f'*/wasm-tools-{wasm_version}/Cargo.toml'))
    if len(wasm_sources) != 1:
        raise ValueError('expected one wasm-tools source for attribution')
    notices = cache / 'notices'
    if notices.exists():
        shutil.rmtree(notices)
    run([sys.executable, source / 'scripts/toolchain-notices.py', '--wasm-tools-source', wasm_sources[0].parent,
         '--runtime-work', ROOT / 'vendor', '--wasi-sdk', runtime_work / 'wasi-sdk',
         '--managed-notices', ROOT / '.build/clr-anycpu/release/notices', '--target', 'x86_64-pc-windows-msvc', '--output', notices], 'notices')
    toolchain = cache / 'toolchain'
    if toolchain.exists():
        shutil.rmtree(toolchain)
    run([sys.executable, source / 'scripts/package-toolchain.py', '--runtime-output', runtime_output,
         '--wasi-sdk', runtime_work / 'wasi-sdk', '--wasm-tools', cache / 'tools/bin/wasm-tools.exe',
         '--native-library', native, '--dotnet-root', sdk, '--notices', notices,
         '--cli-package', packages / f'Isoplane.Cli.{version}.nupkg',
         '--templates', packages / f'Isoplane.Templates.{version}.nupkg',
         '--mono-targets', mono, '--wasi-cross', cross, '--output', toolchain], 'assemble')
    bundle = cache / 'bundle'
    if bundle.exists():
        shutil.rmtree(bundle)
    msi = cache / 'sdk.msi'
    msi.unlink(missing_ok=True)
    run([sys.executable, source / 'scripts/package-windows.py', '--toolchain', toolchain,
         '--version', version, '--output', msi, '--bundle', bundle, '--vc-redist', redist,
         '--packages', packages, '--revision', revision, '--source-date-epoch', epoch, '--release', release], 'msi')
    preview.archive_release(release, archive, epoch)
    digest = preview.sha(archive)
    archive.with_name(archive.name + '.sha256').write_text(f'{digest}  {archive.name}\n')
    print(f'Built {archive}\nSHA256 {digest}', flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cache', type=Path, default=None)
    parser.add_argument('--output', type=Path, default=ROOT / '.build/releases')
    parser.add_argument('--msys-root', type=Path, default=Path('C:/msys64'))
    parser.add_argument('--vc-redist', type=Path)
    parser.add_argument('--runtime-modules', type=Path, help='use shared WASM modules from this SDK revision')
    parser.add_argument('--check-prerequisites', action='store_true', help='check local build tools without starting a build')
    args = parser.parse_args()
    for path in (args.cache, args.output):
        if path is not None and not path.resolve().is_relative_to(ROOT):
            parser.error('build and release directories must be inside this worktree')
    if platform.system() != 'Windows' or platform.machine().lower() not in ('amd64', 'x86_64'):
        parser.error('run this builder on Windows x64')

    if args.vc_redist and not args.vc_redist.is_file():
        parser.error(f'Visual C++ redistributable not found: {args.vc_redist}')
    command = ['powershell.exe', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File',
               str(ROOT / 'scripts/setup-windows.ps1'), '-MsysRoot', str(args.msys_root.resolve())]
    if args.check_prerequisites:
        command.append('-CheckOnly')
    subprocess.run(command, check=True)
    import winreg
    paths = [str(Path(sys.executable).parent)]
    for hive, key in [(winreg.HKEY_LOCAL_MACHINE, r'SYSTEM\CurrentControlSet\Control\Session Manager\Environment'),
                      (winreg.HKEY_CURRENT_USER, r'Environment')]:
        try:
            with winreg.OpenKey(hive, key) as registry:
                value, _ = winreg.QueryValueEx(registry, 'Path')
                paths.append(os.path.expandvars(value))
        except FileNotFoundError:
            continue
    paths.extend([os.environ.get('PATH', ''), str(Path.home() / '.cargo/bin')])
    os.environ['PATH'] = os.pathsep.join(paths)
    rust_version = re.search(r'^ARG RUST_VERSION=(\S+)', (ROOT / 'Dockerfile').read_text(), re.MULTILINE).group(1)
    os.environ['RUSTUP_TOOLCHAIN'] = rust_version + '-x86_64-pc-windows-msvc'
    if args.check_prerequisites:
        return
    with load('build_storage').workspace('release-win-x64', args.cache) as cache:
        args.cache = cache
        build(args)


if __name__ == '__main__':
    main()
