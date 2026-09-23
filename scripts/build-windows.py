#!/usr/bin/env python3
"""Build a Windows x64 SDK release locally, from source or a verified runtime artifact."""
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
import xml.etree.ElementTree as ET
import zipfile

ROOT = Path(__file__).resolve().parents[1]


def load(name):
    path = ROOT / 'scripts' / (name + '.py')
    spec = importlib.util.spec_from_file_location(name.replace('-', '_'), path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
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
        identity = load('build-preview').load(ROOT / 'guest-runtime/feed-identity.py')
        identity.verify_unpacked(archive, destination)
        return destination
    url = f'https://api.nuget.org/v3-flatcontainer/{name.lower()}/{version}/{name.lower()}.{version}.nupkg'
    with urllib.request.urlopen(url, timeout=60) as response, archive.open('wb') as stream:
        shutil.copyfileobj(response, stream)
    with zipfile.ZipFile(archive) as package:
        package.extractall(destination)
    return destination



def prepare_llvm(cache, source, run):
    preview = load('build-preview')
    manifest = json.loads((source / 'guest-runtime/compiler/manifest.json').read_text())
    version = manifest['llvm_version']
    directory = cache / ('llvm-' + version)
    receipt = directory / 'isoplane-build.json'
    configuration = ['-DCMAKE_C_COMPILER=cl', '-DCMAKE_CXX_COMPILER=cl',
        '-DCMAKE_BUILD_TYPE=Release', '-DLLVM_INCLUDE_BENCHMARKS=OFF', '-DLLVM_ENABLE_TERMINFO=OFF',
        '-DLLVM_ENABLE_ZSTD=OFF', '-DLLVM_ENABLE_ZLIB=OFF', '-DLLVM_ENABLE_LIBXML2=OFF',
        '-DLLVM_TARGETS_TO_BUILD=WebAssembly', '-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded']
    expected = {'sourceSha256': manifest['llvm_source_sha256'], 'configuration': configuration}
    if receipt.exists() and json.loads(receipt.read_text()) == expected:
        return directory
    archive = cache / f'llvm-project-{version}.src.tar.xz'
    preview.download(f'https://github.com/llvm/llvm-project/releases/download/llvmorg-{version}/{archive.name}', archive,
                     expected['sourceSha256'])
    sources = cache / f'llvm-project-{version}.src'
    if sources.exists():
        shutil.rmtree(sources)
    with tarfile.open(archive) as bundle:
        bundle.extractall(cache, filter='data')
    run(['cmake', '--fresh', '-G', 'Ninja', '-S', sources / 'llvm', '-B', directory,
         *configuration], 'llvm-configure')
    run(['cmake', '--build', directory, '--parallel', str(min(os.cpu_count() or 1, 8)),
         '--target', 'LLVMCore', 'LLVMBitWriter', 'llvm-config'], 'llvm-build')
    receipt.write_text(json.dumps(expected, indent=2) + '\n')
    return directory


def build(args):
    preview = load('build-preview')
    load('package-debugger').verify(args.debugger)
    revision = preview.source_revision(ROOT)
    epoch = int(preview.output(['git', 'show', '-s', '--format=%ct', revision]))
    version = preview.release_version(ROOT)
    cache = args.cache.resolve()
    logs = cache / 'logs'
    logs.mkdir(parents=True, exist_ok=True)
    releases = args.output.resolve()
    releases.mkdir(parents=True, exist_ok=True)
    release = releases / f'isoplane-win-x64-{version}'
    archive = release.with_name(release.name + '.tgz')
    receipt = releases / f'{release.name}.receipt.json'
    if receipt.exists() and json.loads(receipt.read_text()).get('runtimeArtifactSha256') != args.runtime_sha256:
        raise ValueError('this release was built with different runtime inputs; use a new output directory')
    if preview.existing_release(release, archive, receipt, revision):
        print(f'Verified existing release: {release}')
        return
    source = cache / 'source'
    preview.export_source(ROOT, source, cache, revision, logs)
    runtime = source / 'guest-runtime'
    environment, visual_studio = developer_environment(args.msys_root.resolve())
    redist = redistributable(visual_studio, args.vc_redist)
    sdk, sdk_identity = preview.prepare_sdk(source, cache, logs, 'win-x64')
    environment.update(DOTNET_ROOT=str(sdk), DOTNET_CLI_HOME=str(cache / 'dotnet-home'),
        NUGET_PACKAGES=str(cache / 'nuget'), DOTNET_CLI_TELEMETRY_OPTOUT='1', DOTNET_NOLOGO='1',
        CARGO_TARGET_DIR=str(cache / 'target'), CARGO_BUILD_BUILD_DIR=str(cache / 'target/build'))
    environment['PATH'] = str(sdk) + os.pathsep + environment['PATH']

    def run(command, name):
        preview.run(command, source, logs / (name + '.log'), environment)

    runtime_output = runtime / 'out'
    if args.runtime_artifact:
        imported = cache / 'imported-runtime'
        if imported.exists():
            shutil.rmtree(imported)
        load('runtime-artifact').import_artifact(args.runtime_artifact, args.runtime_sha256, imported)
        runtime_output.mkdir(exist_ok=True)
        for name in ('feed', 'packs'):
            if (runtime_output / name).exists():
                shutil.rmtree(runtime_output / name)
            shutil.move(imported / name, runtime_output / name)
        imported.rmdir()
        stages = ['acquire', 'toolchain', 'patch']
    else:
        try:
            preview.load(runtime / 'feed-identity.py').verify_feed(runtime_output / 'feed', runtime)
            stages = ['acquire', 'toolchain', 'patch']
        except (OSError, ValueError, ET.ParseError, zipfile.BadZipFile) as error:
            print(f'Runtime build required: {error}', flush=True)
            stages = []
    try:
        run(runtime_command(args.msys_root.resolve(), runtime, stages), 'runtime')
    finally:
        runtime_logs = runtime / 'work/logs'
        if runtime_logs.is_dir():
            shutil.copytree(runtime_logs, logs / 'runtime-stages', dirs_exist_ok=True)
    compiler = runtime_output / 'compiler'
    try:
        preview.load(runtime / 'compiler/build.py').verify(compiler, True)
        print(f'Reusing verified compiler: {compiler}', flush=True)
    except (OSError, ValueError, KeyError, zipfile.BadZipFile) as error:
        print(f'Compiler build required: {error}', flush=True)
        llvm = args.llvm.resolve() if args.llvm else prepare_llvm(cache, source, run)
        if compiler.exists():
            shutil.rmtree(compiler)
        try:
            run([sys.executable, runtime / 'compiler/build.py', 'build', '--llvm', llvm,
                 '--output', compiler], 'compiler')
        finally:
            for compiler_log in (runtime / 'work/compiler').glob('*.log'):
                shutil.copyfile(compiler_log, logs / ('compiler-' + compiler_log.name))
    run(['cargo', 'build', '--locked', '--release', '-p', 'isoplane-native'], 'native')
    native = cache / 'target/release/isoplane_native.dll'
    wasm_version = re.search(r'cargo install --locked wasm-tools@([^\s]+)', (source / 'Dockerfile').read_text()).group(1)
    run(['cargo', 'install', '--locked', f'wasm-tools@{wasm_version}', '--root', cache / 'tools'], 'wasm-tools')
    packages = cache / 'packages'
    if packages.exists():
        shutil.rmtree(packages)
    packages.mkdir()
    for name, project in preview.PROJECTS.items():
        run([sdk / 'dotnet.exe', 'pack', source / project, '-c', 'Release', '-o', packages,
             f'-p:RepositoryCommit={revision}', f'-p:SourceRevisionId={revision}',
             '-p:EnableSourceControlManagerQueries=false', '-p:ContinuousIntegrationBuild=true',
             f'-p:IsoplaneNativePath={native}'], 'pack-' + name)
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
         '--runtime-work', runtime / 'work', '--wasi-sdk', runtime / 'work/wasi-sdk',
         '--managed-source', source, '--target', 'x86_64-pc-windows-msvc', '--output', notices], 'notices')
    toolchain = cache / 'toolchain'
    if toolchain.exists():
        shutil.rmtree(toolchain)
    run([sys.executable, source / 'scripts/package-toolchain.py', '--runtime-output', runtime_output, '--debugger', args.debugger.resolve(),
         '--wasi-sdk', runtime / 'work/wasi-sdk', '--wasm-tools', cache / 'tools/bin/wasm-tools.exe',
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
    receipt.write_text(json.dumps({'sourceRevision': revision, 'files': preview.inventory(release),
                                  'archiveSha256': digest, 'dotnetSdk': sdk_identity,
                                  'runtimeArtifactSha256': args.runtime_sha256}, indent=2) + '\n')
    shared = releases / f'isoplane-runtime-{revision}.tgz'
    if not shared.exists():
        load('runtime-artifact').export_artifact(runtime_output, shared)
    print(f'Built {archive}\nSHA256 {digest}', flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--debugger', type=Path, help='verified package-debugger.py output')
    parser.add_argument('--cache', type=Path, default=ROOT / '.isoplane-dev/windows-build')
    parser.add_argument('--output', type=Path, default=ROOT / '.isoplane-dev/releases')
    parser.add_argument('--msys-root', type=Path, default=Path('C:/msys64'))
    parser.add_argument('--llvm', type=Path, help='reuse a matching LLVM development build; otherwise build the pinned LLVM sources')
    parser.add_argument('--vc-redist', type=Path)
    parser.add_argument('--runtime-artifact', help='shared runtime archive file or HTTPS URL; omitted to build from source')
    parser.add_argument('--runtime-sha256', help='required checksum for --runtime-artifact')
    parser.add_argument('--check-prerequisites', action='store_true', help='check local build tools without starting a build')
    args = parser.parse_args()
    if not args.check_prerequisites and not args.debugger:
        parser.error('--debugger is required when building a release')
    if bool(args.runtime_artifact) != bool(args.runtime_sha256):
        parser.error('--runtime-artifact and --runtime-sha256 must be supplied together')
    if args.runtime_sha256:
        if not re.fullmatch('[a-fA-F0-9]{64}', args.runtime_sha256):
            parser.error('--runtime-sha256 must contain 64 hexadecimal digits')
        args.runtime_sha256 = args.runtime_sha256.lower()
    if platform.system() != 'Windows' or platform.machine().lower() not in ('amd64', 'x86_64'):
        parser.error('run this builder on Windows x64')
    if args.llvm and not (args.llvm / 'bin/llvm-config.exe').is_file():
        parser.error(f'LLVM development build not found: {args.llvm}')
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
    args.cache.mkdir(parents=True, exist_ok=True)
    with (args.cache / '.lock').open('a+b') as lock:
        load('fixture-tools').lock_file(lock)
        build(args)


if __name__ == '__main__':
    main()
