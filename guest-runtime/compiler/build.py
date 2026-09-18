#!/usr/bin/env python3
"""Build and verify the pinned NativeAOT compiler and native ABI backend from source."""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tempfile
import urllib.request
import xml.etree.ElementTree as ET
import zipfile

HERE = Path(__file__).resolve().parent
BACKEND = 'tools/libclrjit_universal_llvm32_x64.so'


def digest(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def manifest():
    result = json.loads((HERE / 'manifest.json').read_text())
    source = (HERE.parent / 'manifest.env').read_text()
    for key in ('RUNTIMELAB_COMMIT', 'RUNTIMELAB_TARBALL_URL', 'RUNTIMELAB_TARBALL_SHA256'):
        result[key] = re.search(r'^' + key + r'=(.+)$', source, re.MULTILINE).group(1)
    return result


def identity():
    hashes = {str(p.relative_to(HERE.parent)): digest(p) for p in [HERE.parent / 'source-tree.py', HERE / 'build.py', HERE / 'manifest.json', *sorted((HERE / 'patches').glob('*.patch'))]}
    payload = {'manifest': manifest(), 'files': hashes}
    return hashlib.sha256(json.dumps(payload, sort_keys=True).encode()).hexdigest()


def fetch(url, path, expected):
    if not path.exists() or digest(path) != expected:
        path.parent.mkdir(parents=True, exist_ok=True)
        with urllib.request.urlopen(url) as response, path.open('wb') as output:
            shutil.copyfileobj(response, output)
    if digest(path) != expected:
        raise ValueError(f'input hash mismatch: {path}')


def props_bytes(version, sha512, source_identity, rid):
    root = ET.Element('Project')
    group = ET.SubElement(root, 'PropertyGroup')
    for key, value in {'IsoplaneGuestCompilerVersion': version, 'IsoplaneGuestCompilerSha512': sha512,
                       'IsoplaneGuestCompilerInputsSha256': source_identity,
                       'IsoplaneGuestCompilerRuntimeIdentifier': rid}.items():
        ET.SubElement(group, key).text = value
    return ET.tostring(root, encoding='utf-8', xml_declaration=True)


def verify(output, current):
    receipt = json.loads((output / 'compiler.json').read_text())
    if current and receipt['inputs_sha256'] != identity():
        raise ValueError('compiler is stale; run guest-runtime/compiler/build.py build')
    package = output / receipt['package']
    if Path(receipt['package']).name != receipt['package'] or digest(package) != receipt['package_sha256']:
        raise ValueError('compiler package does not match its receipt')
    with package.open('rb') as stream:
        actual_sha512 = base64.b64encode(hashlib.file_digest(stream, 'sha512').digest()).decode()
    if actual_sha512 != receipt['package_sha512']:
        raise ValueError('compiler package SHA512 does not match its receipt')
    expected = props_bytes(receipt['version'], receipt['package_sha512'], receipt['inputs_sha256'], receipt['upstream']['runtime_identifier'])
    if (output / 'Isoplane.GuestCompiler.props').read_bytes() != expected:
        raise ValueError('compiler MSBuild properties do not match its receipt')
    if digest(output / 'backend') != receipt['backend_sha256']:
        raise ValueError('compiler dependency probe backend does not match its receipt')
    with zipfile.ZipFile(package) as archive:
        if hashlib.sha256(archive.read(BACKEND)).hexdigest() != receipt['backend_sha256']:
            raise ValueError('compiler backend does not match its receipt')
    with zipfile.ZipFile(package) as archive:
        for name, expected_hash in receipt['compiler_files'].items():
            if hashlib.sha256(archive.read(name)).hexdigest() != expected_hash:
                raise ValueError(f'compiler file does not match its receipt: {name}')
    return receipt


def compose(upstream, backend, managed, output):
    spec = manifest()
    if digest(upstream) != spec['sha256']:
        raise ValueError('upstream compiler package hash mismatch')
    backend_hash = digest(backend)
    files = {f'tools/{path.name}': path for path in sorted(managed.iterdir()) if path.is_file() and path.suffix != '.pdb'}
    if 'tools/ilc' not in files:
        raise ValueError('source-built compiler executable is missing')
    if BACKEND not in files or digest(files[BACKEND]) != backend_hash:
        raise ValueError('source-built compiler uses a different native backend')
    compiler_files = {name: digest(path) for name, path in files.items()}
    payload_hash = hashlib.sha256(json.dumps(compiler_files, sort_keys=True).encode()).hexdigest()
    version = f"10.0.0-isoplane.{identity()[:12]}.{payload_hash[:12]}"
    if output.exists():
        raise ValueError(f'output already exists: {output}; choose a new compiler output directory')
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.compiler-', dir=output.parent) as temporary:
        stage = Path(temporary)
        package = stage / f"{spec['package']}.{version}.nupkg"
        with zipfile.ZipFile(upstream) as original, zipfile.ZipFile(package, 'w', compression=zipfile.ZIP_DEFLATED) as result:
            if len(original.namelist()) != len(set(original.namelist())):
                raise ValueError('duplicate upstream compiler entries')
            for entry in original.infolist():
                if entry.filename == '.signature.p7s':
                    continue
                data = original.read(entry)
                if entry.filename in files:
                    data = files[entry.filename].read_bytes()
                elif entry.filename.endswith('.nuspec'):
                    root = ET.fromstring(data)
                    ns = {'n': root.tag.split('}')[0][1:]}
                    ET.register_namespace('', ns['n'])
                    repository = root.find('./n:metadata/n:repository', ns)
                    if repository.get('commit') != spec['RUNTIMELAB_COMMIT']:
                        raise ValueError('managed compiler and backend sources differ')
                    root.find('./n:metadata/n:version', ns).text = version
                    data = ET.tostring(root, encoding='utf-8', xml_declaration=True)
                result.writestr(entry, data)
            for name, path in files.items():
                if name not in original.namelist():
                    entry = zipfile.ZipInfo(name)
                    entry.external_attr = (0o100755 if path.name == 'ilc' else 0o100644) << 16
                    result.writestr(entry, path.read_bytes())
            result.writestr(zipfile.ZipInfo('ISOPLANE-COMPILER-NOTICE.txt'), 'Isoplane builds the managed compiler and LLVM backend from the pinned runtime source with the accompanying patches. Upstream license notices remain included.\n')
            for patch in sorted((HERE / 'patches').glob('*.patch')):
                result.writestr(zipfile.ZipInfo('isoplane-patches/' + patch.name), patch.read_bytes())
        with package.open('rb') as stream:
            sha512 = base64.b64encode(hashlib.file_digest(stream, 'sha512').digest()).decode()
        receipt = {'inputs_sha256': identity(), 'version': version, 'package': package.name,
                   'package_sha256': digest(package), 'package_sha512': sha512,
                   'backend_sha256': backend_hash, 'compiler_files': compiler_files, 'upstream': spec}
        shutil.copyfile(backend, stage / 'backend')
        (stage / 'compiler.json').write_text(json.dumps(receipt, indent=2) + '\n')
        (stage / 'Isoplane.GuestCompiler.props').write_bytes(props_bytes(version, sha512, identity(), spec['runtime_identifier']))
        stage.rename(output)
    return verify(output, True)


def build(args):
    spec = manifest()
    if platform.system() != 'Linux' or platform.machine() != 'x86_64':
        raise ValueError('this compiler backend build currently qualifies Linux x64 only')
    llvm_config = args.llvm / 'bin/llvm-config'
    version = subprocess.check_output([str(llvm_config), '--version'], text=True).strip()
    if version != spec['llvm_version']:
        raise ValueError(f"LLVM {spec['llvm_version']} is required; found {version}")
    args.work.mkdir(parents=True, exist_ok=True)
    archive = args.work / 'runtimelab.tar.gz'
    fetch(spec['RUNTIMELAB_TARBALL_URL'], archive, spec['RUNTIMELAB_TARBALL_SHA256'])
    tree = args.work / 'source'
    subprocess.run(['python3', str(HERE.parent / 'source-tree.py'), 'materialize', str(tree), str(archive), spec['RUNTIMELAB_TARBALL_SHA256'], str(HERE / 'patches')], check=True)
    environment = os.environ.copy()
    environment['LLVM_CMAKE_CONFIG'] = str(args.llvm / 'lib/cmake/llvm')
    command = ['./src/coreclr/build-runtime.sh', '-x64', '-release', '-os', 'linux', '-ninja', '-component', 'llvmjit', '-cmakeargs', '-DFEATURE_EVENTSOURCE_XPLAT=0']
    with (args.work / 'build.log').open('w') as log:
        subprocess.run(command, cwd=tree, env=environment, stdout=log, stderr=subprocess.STDOUT, check=True)
    with tempfile.TemporaryDirectory(prefix='managed-publish-', dir=args.work) as published:
        command = ['./dotnet.sh', 'publish', 'src/coreclr/tools/aot/ILCompiler/ILCompiler_publish.csproj', '-c', 'Release',
                   '-p:TargetArchitecture=x64', '-p:TargetOS=linux', '-p:UseNativeAotForComponents=false', '-p:UseBootstrap=false', f'-p:PublishDir={published}/']
        with (args.work / 'managed-build.log').open('w') as log:
            subprocess.run(command, cwd=tree, env=environment, stdout=log, stderr=subprocess.STDOUT, check=True)
        upstream = args.work / 'upstream.nupkg'
        with urllib.request.urlopen(spec['source']) as response:
            index = json.load(response)
        base = next(resource['@id'] for resource in index['resources'] if resource['@type'].startswith('PackageBaseAddress/'))
        name = spec['package'].lower()
        fetch(f"{base}{name}/{spec['version']}/{name}.{spec['version']}.nupkg", upstream, spec['sha256'])
        return compose(upstream, tree / 'artifacts/bin/coreclr/linux.x64.Release/libclrjit_universal_llvm32_x64.so',
                       Path(published), args.output)



def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='action', required=True)
    for action in ('verify-current', 'verify-artifact'):
        item = sub.add_parser(action)
        item.add_argument('--output', type=Path, required=True)
    item = sub.add_parser('build')
    item.add_argument('--output', type=Path, default=HERE.parent / 'out/compiler')
    item.add_argument('--work', type=Path, default=HERE.parent / 'work/compiler')
    item.add_argument('--llvm', type=Path, default=Path('/usr/lib/llvm-18'))
    args = parser.parse_args()
    try:
        result = build(args) if args.action == 'build' else verify(args.output, args.action == 'verify-current')
        print(result['version'])
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f'compiler: {error}\n')


if __name__ == '__main__':
    main()
