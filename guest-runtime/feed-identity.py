"""Identify runtime inputs and validate a published runtime feed before use."""
import argparse
import base64
import hashlib
import re
from pathlib import Path
import xml.etree.ElementTree as ET
import zipfile


def input_digest(root):
    root = Path(root)
    paths = [root / name for name in ('manifest.env', 'build.sh', 'source-tree.py', 'feed-identity.py')]
    paths.extend(sorted(path for path in (root / 'patches').rglob('*') if path.is_file()))
    digest = hashlib.sha256()
    for path in paths:
        name = path.relative_to(root).as_posix().encode()
        content = path.read_bytes()
        digest.update(len(name).to_bytes(8, 'big'))
        digest.update(name)
        digest.update(len(content).to_bytes(8, 'big'))
        digest.update(content)
    return digest.hexdigest()


def file_digest(stream, algorithm):
    digest = hashlib.new(algorithm)
    for chunk in iter(lambda: stream.read(1024 * 1024), b''):
        digest.update(chunk)
    return digest.digest()


def verify_unpacked(package, directory):
    with zipfile.ZipFile(package) as archive:
        expected_paths = {entry.filename for entry in archive.infolist() if not entry.is_dir()}
        actual_paths = set()
        for path in directory.rglob('*'):
            if path.is_symlink():
                raise ValueError(f'unpacked runtime package contains a link: {path}')
            if path.is_file():
                actual_paths.add(path.relative_to(directory).as_posix())
        if actual_paths != expected_paths:
            raise ValueError(f'unpacked runtime package inventory differs: {directory}')
        for entry in archive.infolist():
            if entry.is_dir():
                continue
            relative = Path(entry.filename)
            if relative.is_absolute() or '..' in relative.parts:
                raise ValueError(f'runtime package contains an invalid path: {entry.filename}')
            path = directory / relative
            if path.is_symlink() or not path.is_file():
                raise ValueError(f'unpacked runtime package file is missing or linked: {path}')
            with archive.open(entry) as expected, path.open('rb') as actual:
                if file_digest(expected, 'sha256') != file_digest(actual, 'sha256'):
                    raise ValueError(f'unpacked runtime package content differs: {path}')


def verify_feed(feed, current_root=None):
    feed = Path(feed)
    props = ET.parse(feed / 'Isoplane.GuestRuntime.props').getroot()

    def field(name):
        elements = props.findall('./PropertyGroup/' + name)
        if len(elements) > 1:
            raise ValueError(f'{feed} has ambiguous duplicate {name} properties')
        value = elements[0].text if elements else None
        if not value or not value.strip():
            raise ValueError(f'{feed} has no {name}; rebuild the runtime feed')
        return value.strip()

    identity = field('IsoplaneGuestRuntimeInputsSha256')
    if re.fullmatch(r'[0-9a-f]{64}', identity) is None:
        raise ValueError('runtime input identity is not a lowercase SHA256 digest')
    if current_root is not None and identity != input_digest(current_root):
        raise ValueError('runtime feed is stale for current sources; run guest-runtime/build.sh')
    packages = (
        ('Microsoft.NETCore.App.Runtime.Mono.wasi-wasm', 'IsoplaneGuestRuntimePack'),
        ('Microsoft.NET.Runtime.WebAssembly.Wasi.Sdk', 'IsoplaneGuestWasiSdkPack'),
    )
    for package, prefix in packages:
        version = field(prefix + 'Version')
        if Path(version).name != version or '\\' in version:
            raise ValueError(f'invalid runtime package version: {version}')
        path = feed / f'{package}.{version}.nupkg'
        with path.open('rb') as stream:
            actual = base64.b64encode(file_digest(stream, 'sha512')).decode()
        if actual != field(prefix + 'Sha512'):
            raise ValueError(f'runtime package content differs from published identity: {path}')
        directory = field(prefix + 'Directory').replace('$(MSBuildThisFileDirectory)', str(feed.resolve()) + '/')
        if '$(' in directory:
            raise ValueError(f'unsupported runtime directory expression: {directory}')
        verify_unpacked(path, Path(directory))
    return identity


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='action', required=True)
    digest = commands.add_parser('digest')
    digest.add_argument('root', type=Path)
    current = commands.add_parser('verify-current')
    current.add_argument('root', type=Path)
    current.add_argument('feed', type=Path)
    artifact = commands.add_parser('verify-artifact')
    artifact.add_argument('feed', type=Path)
    args = parser.parse_args()
    try:
        if args.action == 'digest':
            print(input_digest(args.root))
        elif args.action == 'verify-current':
            print(verify_feed(args.feed, args.root))
        else:
            print(verify_feed(args.feed))
    except (OSError, ValueError, ET.ParseError, zipfile.BadZipFile) as error:
        parser.exit(1, f'{error}\n')


if __name__ == '__main__':
    main()
