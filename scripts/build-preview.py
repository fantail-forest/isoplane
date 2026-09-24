#!/usr/bin/env python3
"""Build the complete Linux or macOS developer preview from committed HEAD at the declared release version.

    python3 scripts/build-preview.py [--cache DIR] [--output DIR]

The result is one release directory (release.json, the package feed, the
toolchain archive, samples and README), plus its archive, checksum and build
receipt under --output (default .isoplane-dev/releases). Publishing the release
directory under a release origin is what isoplane setup downloads from.
Rebuilding the same version from the same revision verifies the existing
release; from another revision it is refused until the version is moved with
scripts/release-version.py.
"""
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


def load(path):
    spec = importlib.util.spec_from_file_location(path.stem.replace('-', '_'), path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def sha(path, algorithm='sha256'):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, algorithm).hexdigest()


def output(command, cwd=ROOT):
    return subprocess.check_output([str(arg) for arg in command], cwd=cwd, text=True).strip()


def run(command, cwd, log, env=None):
    print(f'{log.stem}: {log}', flush=True)
    command = [str(arg) for arg in command]
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


def inventory(directory):
    return [{'path': p.relative_to(directory).as_posix(), 'sha256': sha(p),
             'executable': bool(p.stat().st_mode & 0o111)}
            for p in sorted(directory.rglob('*')) if p.is_file()]


def verify_inventory(directory, expected):
    if inventory(directory) != expected:
        raise ValueError(f'cached artifact inventory or content differs: {directory}')


def download(url, destination, expected, algorithm='sha256'):
    if destination.exists() and sha(destination, algorithm) == expected:
        return
    with urllib.request.urlopen(url, timeout=60) as response, destination.open('wb') as stream:
        shutil.copyfileobj(response, stream)
    if sha(destination, algorithm) != expected:
        raise ValueError(f'download hash differs: {destination}')


def export_git_tree(root, revision, destination, archive, log):
    run(['git', 'archive', '--format=tar', '-o', archive, revision], root, log)
    with tarfile.open(archive) as tree:
        tree.extractall(destination, filter='data')
    archive.unlink()
    entries = subprocess.check_output(['git', 'ls-tree', '-r', '-z', revision], cwd=root)
    for entry in entries.split(b'\0'):
        if not entry:
            continue
        metadata, name = entry.split(b'\t', 1)
        mode, _, commit = metadata.decode().split()
        if mode != '160000':
            continue
        path = Path(os.fsdecode(name))
        repository = root / path
        if not (repository / '.git').exists():
            raise ValueError(f'initialize source dependencies with git submodule update --init --recursive: {path}')
        target = destination / path
        target.mkdir(parents=True, exist_ok=True)
        export_git_tree(repository, commit, target, archive, log)


def export_source(root, source, cache, revision, logs):
    preserved = cache / 'runtime-build'
    preserved.mkdir(exist_ok=True)
    for name in ('work', 'out'):
        path = source / 'guest-runtime' / name
        if path.exists():
            path.rename(preserved / name)
    if source.exists():
        shutil.rmtree(source)
    source.mkdir()
    source_tar = cache / 'source.tar'
    export_git_tree(root, revision, source, source_tar, logs / 'source.log')
    for path in preserved.iterdir():
        path.rename(source / 'guest-runtime' / path.name)
    preserved.rmdir()


def prepare_sdk(source, cache, logs, runtime_identifier=RUNTIME_IDENTIFIER):
    version = json.loads((source / 'global.json').read_text())['sdk']['version']
    directory = cache / 'dotnet'
    receipt = cache / 'dotnet.json'
    if receipt.exists():
        identity = json.loads(receipt.read_text())
        if identity['version'] == version and identity.get('runtimeIdentifier', RUNTIME_IDENTIFIER) == runtime_identifier:
            verify_inventory(directory, identity['files'])
            return directory, identity
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
                'files': inventory(directory)}
    receipt.write_text(json.dumps(identity, indent=2) + '\n')
    return directory, identity


def prepare_runtime(source, candidates, logs):
    runtime = source / 'guest-runtime'
    feed = load(runtime / 'feed-identity.py')
    compiler = load(runtime / 'compiler/build.py')
    destination = runtime / 'out'
    destination.mkdir(exist_ok=True)
    for candidate in [source, *candidates]:
        candidate_runtime = candidate / 'guest-runtime'
        try:
            feed.verify_feed(candidate_runtime / 'out/feed', runtime)
        except (OSError, ValueError, ET.ParseError, zipfile.BadZipFile) as error:
            print(f'Runtime cache unavailable at {candidate}: {error}', flush=True)
            continue
        if candidate != source:
            for name in ('feed', 'packs'):
                if (destination / name).exists():
                    shutil.rmtree(destination / name)
                shutil.copytree(candidate_runtime / 'out' / name, destination / name, dirs_exist_ok=True)
        print(f'Reusing verified runtime feed from {candidate}', flush=True)
        break
    else:
        run(['bash', runtime / 'build.sh'], source, logs / 'guest-runtime.log')
    feed.verify_feed(destination / 'feed', runtime)
    for candidate in [source, *candidates]:
        try:
            compiler.verify(candidate / 'guest-runtime/out/compiler', True)
        except (OSError, ValueError, KeyError, zipfile.BadZipFile) as error:
            print(f'Compiler cache unavailable at {candidate}: {error}', flush=True)
            continue
        if candidate != source:
            if (destination / 'compiler').exists():
                shutil.rmtree(destination / 'compiler')
            shutil.copytree(candidate / 'guest-runtime/out/compiler', destination / 'compiler', dirs_exist_ok=True)
        print(f'Reusing verified compiler from {candidate}', flush=True)
        break
    else:
        if (destination / 'compiler').exists():
            shutil.rmtree(destination / 'compiler')
        run(['python3', runtime / 'compiler/build.py', 'build'], source, logs / 'compiler.log')
    compiler.verify(destination / 'compiler', True)
    # Acquisition verifies pinned archives even when a sibling worktree supplies them.
    downloads = runtime / 'work/downloads'
    downloads.mkdir(parents=True, exist_ok=True)
    for candidate in candidates:
        for path in (candidate / 'guest-runtime/work/downloads').glob('*'):
            if path.is_file() and not (downloads / path.name).exists():
                shutil.copyfile(path, downloads / path.name)
    run(['bash', runtime / 'build.sh', 'acquire', 'toolchain', 'patch'], source, logs / 'runtime-sources.log')
    return destination


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


def existing_release(release, archive, receipt_path, revision):
    """Verify a release already assembled at this version, or refuse to overwrite one from another revision."""
    manifest_path = release / 'release.json'
    if manifest_path.exists():
        recorded = json.loads(manifest_path.read_text())['sourceRevision']
        if recorded != revision:
            raise ValueError(f'{release.name} was already assembled from revision {recorded[:12]}; '
                             f'declare a new version with scripts/release-version.py set before building revision {revision[:12]}')
    if receipt_path.exists():
        receipt = json.loads(receipt_path.read_text())
        if receipt['sourceRevision'] != revision:
            raise ValueError(f'{release.name} was already assembled from revision {receipt["sourceRevision"][:12]}; '
                             f'declare a new version with scripts/release-version.py set before building revision {revision[:12]}')
        verify_inventory(release, receipt['files'])
        if sha(archive) != receipt['archiveSha256']:
            raise ValueError('release archive differs from its build receipt')
        if archive.with_name(archive.name + '.sha256').read_text() != f"{receipt['archiveSha256']}  {archive.name}\n":
            raise ValueError('release archive checksum file differs')
        return True
    if release.exists() or archive.exists():
        raise ValueError(f'unreceipted release exists; remove it before retrying: {release}')
    return False


def build(root, cache, releases, logs, args):
    revision = source_revision(root)
    epoch = int(output(['git', 'show', '-s', '--format=%ct', revision], root))
    source = cache / 'source'
    export_source(root, source, cache, revision, logs)
    version = release_version(source)
    if args.runtime_modules:
        load(root / 'scripts/shared-runtime-modules.py').verify(args.runtime_modules.resolve(), version, revision)
    release = releases / release_name(version)
    archive = release.with_name(release.name + '.tgz')
    receipt_path = release.with_name(release.name + '.receipt.json')
    if existing_release(release, archive, receipt_path, revision):
        print(f'Verified existing release: {archive}', flush=True)
        return
    candidates = [Path(line[9:]) for line in output(['git', 'worktree', 'list', '--porcelain'], root).splitlines()
                  if line.startswith('worktree ')]
    runtime_output = (load(source / 'scripts/macos-build.py').prepare(
        load(source / 'scripts/build-preview.py'), source, cache, logs, args) if platform.system() == 'Darwin'
        else prepare_runtime(source, candidates, logs))
    sdk, sdk_identity = prepare_sdk(source, cache, logs, RUNTIME_IDENTIFIER)
    environment = os.environ.copy()
    environment.update(DOTNET_ROOT=str(sdk), DOTNET_CLI_HOME=str(cache / 'dotnet-home'),
                       DOTNET_CLI_TELEMETRY_OPTOUT='1', DOTNET_NOLOGO='1',
                       NUGET_PACKAGES=str(cache / 'nuget'),
                       CARGO_TARGET_DIR=str(cache / 'target'), CARGO_BUILD_BUILD_DIR=str(cache / 'target/build'),
                       PATH=str(sdk) + os.pathsep + os.environ.get('PATH', os.defpath))
    run(['cargo', 'build', '--locked', '--release', '-p', 'isoplane-native'], source, logs / 'native.log', environment)
    native_library = cache / 'target/release' / NATIVE_LIBRARY
    wasm_version = re.search(r'cargo install --locked wasm-tools@([^\s]+)', (source / 'Dockerfile').read_text()).group(1)
    run(['cargo', 'install', '--locked', f'wasm-tools@{wasm_version}', '--root', cache / 'tools'], source,
        logs / 'wasm-tools.log', environment)
    wasm_tools = cache / 'tools/bin/wasm-tools'
    if output([wasm_tools, '--version']) != f'wasm-tools {wasm_version}':
        raise ValueError('installed wasm-tools version differs from the pin')
    runtime_modules = args.runtime_modules.resolve() if args.runtime_modules else cache / 'runtime-modules'
    if not args.runtime_modules:
        if runtime_modules.exists():
            shutil.rmtree(runtime_modules)
        run(['bash', source / 'guest-sdks/dotnet/MonoRuntime/build.sh', '--output', runtime_modules,
             '--source-revision', revision], source,
            logs / 'shared-runtime.log', dict(environment, PATH=str(wasm_tools.parent) + os.pathsep + environment['PATH']))
    load(source / 'scripts/shared-runtime-modules.py').verify(runtime_modules, version, revision, runtime_output / 'feed')
    packages = cache / 'packages'
    if packages.exists():
        shutil.rmtree(packages)
    packages.mkdir()
    for name, project in PROJECTS.items():
        # The version comes from Directory.Build.props alone; only the revision is supplied here.
        run([sdk / 'dotnet', 'pack', source / project, '-c', 'Release', '-o', packages,
             f'-p:RepositoryCommit={revision}', f'-p:SourceRevisionId={revision}',
             '-p:EnableSourceControlManagerQueries=false', '-p:ContinuousIntegrationBuild=true',
             f'-p:IsoplaneNativePath={native_library}', f'-p:IsoplaneRuntimeModuleDirectory={runtime_modules}'],
            source, logs / f'pack-{name}.log', environment)
    verify_packages(packages, version, revision, native_library)
    cargo_home = Path(os.environ.get('CARGO_HOME', Path.home() / '.cargo'))
    wasm_sources = list((cargo_home / 'registry/src').glob(f'*/wasm-tools-{wasm_version}/Cargo.toml'))
    if len(wasm_sources) != 1:
        raise ValueError('expected one downloaded wasm-tools source for attribution')
    notices = cache / 'notices'
    if notices.exists():
        shutil.rmtree(notices)
    run([sys.executable, source / 'scripts/toolchain-notices.py', '--wasm-tools-source', wasm_sources[0].parent,
         '--runtime-work', source / 'guest-runtime/work', '--wasi-sdk', source / 'guest-runtime/work/wasi-sdk',
         '--managed-source', source, '--output', notices], source, logs / 'notices.log', environment)
    run([sys.executable, source / 'scripts/package-toolchain.py', '--runtime-output', runtime_output,
         '--wasi-sdk', source / 'guest-runtime/work/wasi-sdk', '--wasm-tools', wasm_tools,
         '--native-library', native_library, '--dotnet-root', sdk,
         '--notices', notices, '--packages', packages, '--package-version', version,
         '--output', release], source, logs / 'assemble.log', environment)
    verify_packages(release / 'packages', version, revision, native_library)
    runtime_inputs = load(source / 'guest-runtime/feed-identity.py').verify_feed(runtime_output / 'feed', source / 'guest-runtime')
    compiler_identity = load(source / 'guest-runtime/compiler/build.py').verify(runtime_output / 'compiler', True)
    wasi_sdk_version = (source / 'guest-runtime/work/wasi-sdk/VERSION').read_text()
    manifest = {'sourceRevision': revision, 'packageVersion': version, 'runtimeIdentifier': RUNTIME_IDENTIFIER,
                'toolchain': archive_toolchain(release, epoch),
                'packages': [{'id': name, 'version': version, 'file': f'packages/{name}.{version}.nupkg',
                              'sha256': sha(release / 'packages' / f'{name}.{version}.nupkg')} for name in PROJECTS],
                'dotnetSdk': {key: value for key, value in sdk_identity.items() if key != 'files'},
                'wasmToolsVersion': wasm_version,
                'runtimeInputsSha256': runtime_inputs,
                'compiler': compiler_identity,
                'wasiSdk': wasi_sdk_version,
                'builder': {'rustc': output(['rustc', '-vV']), 'cargo': output(['cargo', '--version']),
                            'cargoLockSha256': sha(source / 'Cargo.lock')}}
    (release / 'release.json').write_text(json.dumps(manifest, indent=2) + '\n')
    archive_release(release, archive, epoch)
    checksum = sha(archive)
    archive.with_name(archive.name + '.sha256').write_text(f'{checksum}  {archive.name}\n')
    receipt_path.write_text(json.dumps({'sourceRevision': revision, 'files': inventory(release), 'archiveSha256': checksum}, indent=2) + '\n')
    print(f'Built {archive}\nSHA256 {checksum}', flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--cache', type=Path, default=ROOT / '.isoplane-dev/preview-build',
                        help='build cache shared between releases: SDK download, cargo target, packages')
    parser.add_argument('--output', type=Path, default=ROOT / '.isoplane-dev/releases',
                        help='where the release directory, archive and checksum are written')
    parser.add_argument('--runtime-artifact', help='verified shared runtime archive for a macOS builder')
    parser.add_argument('--runtime-sha256', help='SHA256 of --runtime-artifact')
    parser.add_argument('--runtime-modules', type=Path, help='reuse shared WASM modules and their build receipt from this SDK revision')
    parser.add_argument('--llvm', type=Path, help='LLVM development build matching the compiler pin (macOS)')
    parser.add_argument('--jobs', type=int, default=min(os.cpu_count() or 1, 8))
    args = parser.parse_args()
    if platform.system() == 'Darwin':
        if RUNTIME_IDENTIFIER not in ('osx-arm64', 'osx-x64'):
            parser.error('macOS builds require an ARM64 or x64 host')
        if not args.runtime_artifact or not args.runtime_sha256 or not args.llvm:
            parser.error('macOS builds require --runtime-artifact, --runtime-sha256 and --llvm')
        if not re.fullmatch('[a-fA-F0-9]{64}', args.runtime_sha256):
            parser.error('--runtime-sha256 must contain 64 hexadecimal digits')
        args.runtime_sha256 = args.runtime_sha256.lower()
        args.llvm = args.llvm.resolve()
    elif platform.system() != 'Linux' or platform.machine() != 'x86_64':
        parser.error('build-preview requires Linux x64 or macOS')
    elif args.runtime_artifact or args.runtime_sha256 or args.llvm:
        parser.error('runtime artifact and LLVM options currently apply to macOS builds')
    if args.jobs < 1:
        parser.error('--jobs must be positive')
    os.environ['CARGO_BUILD_JOBS'] = str(args.jobs)
    cache = args.cache.resolve()
    if platform.system() == 'Darwin':
        for variable, directory in [('CARGO_HOME', 'cargo'), ('NUGET_PACKAGES', 'nuget'),
                                    ('TMPDIR', 'tmp'), ('DOTNET_CLI_HOME', 'dotnet-home')]:
            path = cache / directory
            path.mkdir(parents=True, exist_ok=True)
            os.environ.setdefault(variable, str(path))
    releases = args.output.resolve()
    logs = cache / 'logs'
    logs.mkdir(parents=True, exist_ok=True)
    releases.mkdir(parents=True, exist_ok=True)
    with (cache / '.lock').open('w') as lock:
        load(ROOT / 'scripts/fixture-tools.py').lock_file(lock)
        try:
            build(ROOT, cache, releases, logs, args)
        except (ValueError, OSError, subprocess.CalledProcessError) as error:
            raise SystemExit(f'preview build failed: {error}; see {logs}') from error


if __name__ == '__main__':
    main()
