#!/usr/bin/env python3
"""Assemble a Linux or macOS release; Task prepares shared Linux outputs first."""
import argparse
import gzip
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import shutil
import shlex
import subprocess
import sys
import tarfile
import urllib.request
import xml.etree.ElementTree as ET
import zipfile

ROOT = Path(__file__).resolve().parents[1]
RUNTIME_IDENTIFIER = ('osx-' + {'arm64': 'arm64', 'x86_64': 'x64'}.get(platform.machine(), 'unsupported')
                      if platform.system() == 'Darwin' else 'linux-x64')
NATIVE_LIBRARY = 'libisoplane_native.dylib' if platform.system() == 'Darwin' else 'libisoplane_native.so'
PROJECTS = {
    'Isoplane.Sdk': 'guest-sdks/dotnet/Isoplane.Sdk.csproj',
    'Isoplane.AspNetCore': 'guest-sdks/dotnet/AspNetCore/Isoplane.AspNetCore.csproj',
    'Isoplane.Templates': 'guest-sdks/dotnet/Templates/Isoplane.Templates.csproj',
    'Isoplane.Native': 'tools/Isoplane.Native/Isoplane.Native.csproj',
    'Isoplane.Testing': 'tools/Isoplane.Testing/Isoplane.Testing.csproj',
    'Isoplane.Cli': 'tools/Isoplane.Cli/Isoplane.Cli.csproj',
}


def pack_project(name, version, packages, native_library, runtime_identifier):
    """Attach native support to a Linux-built managed package."""
    package = packages / f'{name}.{version}.nupkg'
    shared = ROOT / '.build/clr-anycpu/release/packages' / package.name
    shutil.copy2(shared, package)
    load(ROOT / 'scripts/managed_packages.py').attach_native(package, name, native_library, runtime_identifier)


def load(path):
    if path.name == "build_storage.py" and "build_storage" in sys.modules:
        return sys.modules["build_storage"]
    spec = importlib.util.spec_from_file_location(path.stem.replace('-', '_'), path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    if path.name == "build_storage.py":
        sys.modules["build_storage"] = module
    return module


def sha(path, algorithm='sha256'):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, algorithm).hexdigest()


def output(command, cwd=ROOT):
    return subprocess.check_output([str(arg) for arg in command], cwd=cwd, text=True).strip()


def run(command, cwd, log, env=None):
    print(f'{log.stem}: {log}', flush=True)
    command = [str(arg) for arg in command]
    env = load(ROOT / "scripts/build_storage.py").temporary_environment(
        log.parent / "tmp", os.environ if env is None else env)
    if env is not None:
        command[0] = shutil.which(command[0], path=env.get('PATH')) or command[0]
    with log.open('w') as stream:
        subprocess.run(command, cwd=cwd, env=env,
                       stdout=stream, stderr=subprocess.STDOUT, check=True)


def source_revision(root):
    if output(['git', 'status', '--porcelain', '--untracked-files=normal'], root):
        raise ValueError('commit or remove pending source changes before building a release')
    return output(['git', 'rev-parse', 'HEAD'], root)


def release_version(source):
    versions = load(source / 'scripts/release-version.py')
    drift = versions.check(source)
    if drift:
        listed = ', '.join(f'{path} pins {pinned}' for path, pinned in drift)
        raise ValueError(f'in-repo SDK pins differ from the declared release version: {listed}')
    return versions.declared(source)


def download(url, destination, expected, algorithm='sha256'):
    load(ROOT / 'scripts/build_storage.py').download(url, destination, expected, algorithm)


def prepare_sdk(source, cache, logs, runtime_identifier=RUNTIME_IDENTIFIER):
    version = json.loads((source / 'global.json').read_text())['sdk']['version']
    directory = cache / 'dotnet'
    executable = directory / ('dotnet.exe' if runtime_identifier.startswith('win-') else 'dotnet')
    if executable.is_file():
        installed = subprocess.run([str(executable), '--version'], cwd=source, text=True,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        if installed.returncode == 0 and installed.stdout.strip() == version:
            return directory, {'version': version, 'runtimeIdentifier': runtime_identifier}
    metadata_url = f'https://builds.dotnet.microsoft.com/dotnet/release-metadata/{version.rsplit(".", 1)[0]}/releases.json'
    with urllib.request.urlopen(metadata_url, timeout=60) as response:
        releases = json.load(response)
    sdk = next(sdk for release in releases['releases'] for sdk in release.get('sdks', []) if sdk['version'] == version)
    windows = runtime_identifier.startswith('win-')
    suffix = '.zip' if windows else '.tar.gz'
    archive = next(file for file in sdk['files'] if file['rid'] == runtime_identifier and file['name'].endswith(suffix))
    path = cache / f'dotnet-{version}{suffix}'
    download(archive['url'], path, archive['hash'].lower(), 'sha512')
    if directory.exists():
        shutil.rmtree(directory)
    directory.mkdir()
    if windows:
        with zipfile.ZipFile(path) as bundle:
            bundle.extractall(directory)
    else:
        with tarfile.open(path) as bundle:
            bundle.extractall(directory, filter='data')
    environment = os.environ.copy()
    environment.update(DOTNET_ROOT=str(directory), DOTNET_CLI_HOME=str(cache / 'dotnet-home'),
                       DOTNET_CLI_TELEMETRY_OPTOUT='1', DOTNET_NOLOGO='1')
    if not windows:
        run([directory / 'dotnet', 'workload', 'install', 'wasi-experimental', '--skip-manifest-update',
             '--source', 'https://api.nuget.org/v3/index.json'], source, logs / 'sdk-workload.log', environment)
    identity = {'version': version, 'runtimeIdentifier': runtime_identifier, 'url': archive['url'], 'sha512': archive['hash'].lower(),
                }
    return directory, identity


def package_metadata(package):
    nuspecs = [p for p in package.namelist() if p.endswith('.nuspec')]
    if len(nuspecs) != 1:
        raise ValueError(f'{package.filename} must contain one package identity')
    return ET.fromstring(package.read(nuspecs[0])).find('{*}metadata')


def verify_packages(feed, version, revision, native_library, runtime_identifier=RUNTIME_IDENTIFIER):
    expected = {f'{name}.{version}.nupkg' for name in PROJECTS}
    if {p.name for p in feed.iterdir()} != expected:
        raise ValueError(f'release feed must contain exactly the {len(PROJECTS)} matching packages')
    library_digest = sha(native_library)
    for name in PROJECTS:
        with zipfile.ZipFile(feed / f'{name}.{version}.nupkg') as package:
            metadata = package_metadata(package)
            repository = metadata.find('{*}repository')
            if (metadata.findtext('{*}id') != name or metadata.findtext('{*}version') != version
                    or repository is None or repository.get('commit') != revision):
                raise ValueError(f'{name} package version or source revision differs')
            for dependency in metadata.findall('.//{*}dependency'):
                if dependency.get('id') in PROJECTS and dependency.get('version').strip('[]') != version:
                    raise ValueError(f'{name} references a different release package')
            if name in ('Isoplane.Sdk', 'Isoplane.AspNetCore'):
                props = ET.fromstring(package.read('Sdk/Sdk.props'))
                reference = props.find(f".//PackageReference[@Include='{name}']")
                if (reference is None or reference.get('Version') != version
                        or props.findtext('./PropertyGroup/IsoplaneSdkVersion') != version):
                    raise ValueError(f'{name} SDK selects a different package version')
            if name == 'Isoplane.Templates':
                project = ET.fromstring(package.read('content/content/aspnet/SampleApp.csproj'))
                if project.get('Sdk') != f'Isoplane.AspNetCore/{version}':
                    raise ValueError('template selects a different SDK version')
            if name == 'Isoplane.Native':
                entry = f'runtimes/{runtime_identifier}/native/{native_library.name}'
                if entry not in package.namelist() or hashlib.sha256(package.read(entry)).hexdigest() != library_digest:
                    raise ValueError('Isoplane.Native carries a native library other than the one built for this release')
    load(ROOT / 'scripts/package-toolchain.py').verify_native_pair(feed / f'Isoplane.Cli.{version}.nupkg', native_library)


def archive_directory(directory, archive, epoch, prefix):
    """Archive a directory reproducibly: entries are ordered, owned by root and dated at the source commit."""
    with archive.open('wb') as raw, gzip.GzipFile(filename='', fileobj=raw, mode='wb', mtime=epoch) as compressed:
        with tarfile.open(fileobj=compressed, mode='w') as bundle:
            for path in sorted(directory.rglob('*')) if not prefix else [directory, *sorted(directory.rglob('*'))]:
                info = bundle.gettarinfo(str(path), arcname=str(Path(prefix) / path.relative_to(directory)))
                info.uid = info.gid = 0
                info.uname = info.gname = ''
                info.mtime = epoch
                if path.is_file():
                    with path.open('rb') as stream:
                        bundle.addfile(info, stream)
                else:
                    bundle.addfile(info)


def archive_release(release, archive, epoch):
    archive_directory(release, archive, epoch, release.name)


def archive_toolchain(release, epoch):
    """Pack the assembled toolchain into the archive setup downloads, describe it, and drop the unpacked copy."""
    toolchain = release / 'toolchain'
    archive = release / 'toolchain.tgz'
    archive_directory(toolchain, archive, epoch, '')
    description = {'archive': archive.name, 'archiveSha256': sha(archive), 'manifestSha256': sha(toolchain / 'toolchain.json')}
    shutil.rmtree(toolchain)
    return description


def release_name(version):
    return f'isoplane-{RUNTIME_IDENTIFIER}-{version}'


def existing_release(release, archive, revision):
    manifest = release / 'release.json'
    if manifest.is_file():
        if json.loads(manifest.read_text())['sourceRevision'] != revision:
            raise ValueError(f'{release.name} belongs to another source revision; select a new version or explicitly remove it')
        if archive.is_file():
            return True
    if release.exists() or archive.exists():
        raise ValueError(f'incomplete release exists; explicitly remove it before retrying: {release}')
    return False


def build(root, cache, releases, logs, args):
    revision = source_revision(root)
    epoch = int(output(['git', 'show', '-s', '--format=%ct', revision], root))
    source = root
    version = release_version(source)
    if args.runtime_modules:
        load(root / 'scripts/shared-runtime-modules.py').verify(args.runtime_modules.resolve(), version, revision)
    release = releases / release_name(version)
    archive = release.with_name(release.name + '.tgz')
    if existing_release(release, archive, revision):
        print(f'Verified existing release: {archive}', flush=True)
        return
    runtime_output = source / '.build/wasm32/runtime'
    storage = load(ROOT / 'scripts/build_storage.py')
    runtime_work = storage.build_root(source) / 'development/runtime-work'
    if platform.system() == 'Darwin':
        load(source / 'scripts/macos-build.py').prepare_wasi(
            load(source / 'scripts/build-preview.py'), source, cache, runtime_work)
    sdk, sdk_identity = prepare_sdk(source, cache, logs, RUNTIME_IDENTIFIER)
    environment = os.environ.copy()
    environment.update(DOTNET_ROOT=str(sdk), DOTNET_CLI_HOME=str(cache / 'dotnet-home'),
                       DOTNET_CLI_TELEMETRY_OPTOUT='1', DOTNET_NOLOGO='1',
                       CARGO_TARGET_DIR=str(cache / 'target'), CARGO_BUILD_BUILD_DIR=str(cache / 'target'),
                       PATH=str(sdk) + os.pathsep + os.environ.get('PATH', os.defpath))
    run(['cargo', 'build', '--locked', '--release', '-p', 'isoplane-native'], source, logs / 'native.log', environment)
    native_library = cache / 'target/release' / NATIVE_LIBRARY
    wasm_version = re.search(r'cargo install --locked wasm-tools@([^\s]+)', (source / 'Dockerfile').read_text()).group(1)
    run(['cargo', 'install', '--locked', f'wasm-tools@{wasm_version}', '--root', cache / 'tools'], source,
        logs / 'wasm-tools.log', environment)
    wasm_tools = cache / 'tools/bin/wasm-tools'
    if output([wasm_tools, '--version']) != f'wasm-tools {wasm_version}':
        raise ValueError('installed wasm-tools version differs from the pin')
    runtime_modules = args.runtime_modules.resolve() if args.runtime_modules else source / '.build/wasm32/release-runtime'
    load(source / 'scripts/shared-runtime-modules.py').verify(runtime_modules, version, revision, runtime_output / 'feed')
    packages = cache / 'packages'
    if packages.exists():
        shutil.rmtree(packages)
    packages.mkdir()
    for name in PROJECTS:
        pack_project(name, version, packages, native_library, RUNTIME_IDENTIFIER)
    verify_packages(packages, version, revision, native_library)
    cargo_home = Path(os.environ.get('CARGO_HOME', Path.home() / '.cargo'))
    wasm_sources = list((cargo_home / 'registry/src').glob(f'*/wasm-tools-{wasm_version}/Cargo.toml'))
    if len(wasm_sources) != 1:
        raise ValueError('expected one downloaded wasm-tools source for attribution')
    notices = cache / 'notices'
    if notices.exists():
        shutil.rmtree(notices)
    run([sys.executable, source / 'scripts/toolchain-notices.py', '--wasm-tools-source', wasm_sources[0].parent,
         '--runtime-work', source / 'vendor', '--wasi-sdk', runtime_work / 'wasi-sdk',
         '--managed-notices', ROOT / '.build/clr-anycpu/release/notices', '--output', notices], source, logs / 'notices.log', environment)
    run([sys.executable, source / 'scripts/package-toolchain.py', '--runtime-output', runtime_output,
         '--wasi-sdk', runtime_work / 'wasi-sdk', '--wasm-tools', wasm_tools,
         '--native-library', native_library, '--dotnet-root', sdk,
         '--notices', notices, '--packages', packages, '--package-version', version,
         '--output', release], source, logs / 'assemble.log', environment)
    verify_packages(release / 'packages', version, revision, native_library)
    wasi_sdk_version = (runtime_work / 'wasi-sdk/VERSION').read_text()
    manifest = {'sourceRevision': revision, 'packageVersion': version, 'runtimeIdentifier': RUNTIME_IDENTIFIER,
                'toolchain': archive_toolchain(release, epoch),
                'packages': [{'id': name, 'version': version, 'file': f'packages/{name}.{version}.nupkg',
                              'sha256': sha(release / 'packages' / f'{name}.{version}.nupkg')} for name in PROJECTS],
                'dotnetSdk': {key: value for key, value in sdk_identity.items() if key != 'files'},
                'wasmToolsVersion': wasm_version,
                'wasiSdk': wasi_sdk_version,
                'builder': {'rustc': output(['rustc', '-vV']), 'cargo': output(['cargo', '--version']),
                            'cargoLockSha256': sha(source / 'Cargo.lock')}}
    (release / 'release.json').write_text(json.dumps(manifest, indent=2) + '\n')
    archive_release(release, archive, epoch)
    checksum = sha(archive)
    archive.with_name(archive.name + '.sha256').write_text(f'{checksum}  {archive.name}\n')
    print(f'Built {archive}\nSHA256 {checksum}', flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--assemble', action='store_true', help=argparse.SUPPRESS)
    parser.add_argument('--cache', type=Path, default=None,
                        help='build cache shared between releases: SDK download, cargo target, packages')
    parser.add_argument('--output', type=Path, default=ROOT / '.build/releases',
                        help='where the release directory, archive and checksum are written')
    parser.add_argument('--runtime-modules', type=Path, help='use shared WASM modules from this SDK revision')
    parser.add_argument('--jobs', type=int, default=min(os.cpu_count() or 1, 8))
    args = parser.parse_args()
    for path in (args.cache, args.output):
        if path is not None and not path.resolve().is_relative_to(ROOT):
            parser.error('build and release directories must be inside this worktree')
    if not args.assemble:
        import tasks
        target = 'release:macos' if platform.system() == 'Darwin' else 'release:linux'
        tasks.run(ROOT, target, 'CLI_ARGS=' + shlex.join(sys.argv[1:]))
        return
    if platform.system() == 'Darwin':
        if RUNTIME_IDENTIFIER not in ('osx-arm64', 'osx-x64'):
            parser.error('macOS builds require an ARM64 or x64 host')
    elif platform.system() != 'Linux' or platform.machine() != 'x86_64':
        parser.error('build-preview requires Linux x64 or macOS')
    if args.jobs < 1:
        parser.error('--jobs must be positive')
    os.environ['CARGO_BUILD_JOBS'] = str(args.jobs)
    storage = load(ROOT / 'scripts/build_storage.py')
    with storage.workspace('release-' + RUNTIME_IDENTIFIER, args.cache) as cache:
        releases = args.output.resolve()
        logs = cache / 'logs'
        logs.mkdir(parents=True, exist_ok=True)
        releases.mkdir(parents=True, exist_ok=True)
        try:
            build(ROOT, cache, releases, logs, args)
        except (ValueError, OSError, subprocess.CalledProcessError) as error:
            raise SystemExit(f'preview build failed: {error}; see {logs}') from error


if __name__ == '__main__':
    main()
