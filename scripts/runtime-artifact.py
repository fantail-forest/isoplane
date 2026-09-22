#!/usr/bin/env python3
"""Export a verified runtime build, or import one from a file or HTTPS URL."""

import argparse
import hashlib
import importlib.util
from pathlib import Path, PurePosixPath, PureWindowsPath
import re
import shutil
import tarfile
import tempfile
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]


def identity_module():
    spec = importlib.util.spec_from_file_location('feed_identity', ROOT / 'guest-runtime/feed-identity.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def checksum(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def verify(directory):
    props = ET.parse(directory / 'feed/Isoplane.GuestRuntime.props').getroot()
    for item in props.findall('./PropertyGroup/*'):
        if item.tag.endswith('Directory'):
            value = item.text or ''
            if not re.fullmatch(r'\$\(MSBuildThisFileDirectory\)\.\./packs/[a-z-]+/', value):
                raise ValueError(f'runtime artifact contains an external pack directory: {item.tag}')
    return identity_module().verify_feed(directory / 'feed', ROOT / 'guest-runtime')


def export_artifact(directory, archive):
    verify(directory)
    archive.parent.mkdir(parents=True, exist_ok=True)
    stream = archive.open('xb')
    try:
        with stream, tarfile.open(fileobj=stream, mode='w:gz') as bundle:
            for name in ('feed', 'packs'):
                for path in sorted((directory / name).rglob('*')):
                    if path.is_symlink():
                        raise ValueError(f'runtime artifacts cannot contain links: {path}')
                    if path.is_file():
                        bundle.add(path, arcname=path.relative_to(directory).as_posix(), recursive=False)
    except BaseException:
        archive.unlink()
        raise
    digest = checksum(archive)
    archive.with_name(archive.name + '.sha256').write_text(f'{digest}  {archive.name}\n')
    return digest


def extract(archive, destination):
    with tarfile.open(archive) as bundle:
        seen = set()
        for member in bundle.getmembers():
            name = member.name
            parts = PurePosixPath(name).parts
            if (not parts or parts[0] not in ('feed', 'packs') or '..' in parts
                    or PureWindowsPath(name).drive or '\\' in name or ':' in name
                    or any(part.endswith((' ', '.')) for part in parts)
                    or not (member.isfile() or member.isdir()) or name.casefold() in seen):
                raise ValueError(f'unsafe runtime artifact entry: {name}')
            seen.add(name.casefold())
        bundle.extractall(destination, filter='data')


def import_artifact(source, expected, destination):
    if not re.fullmatch('[a-fA-F0-9]{64}', expected):
        raise ValueError('runtime artifact SHA256 must contain 64 hexadecimal digits')
    if destination.exists():
        raise ValueError(f'runtime artifact destination already exists: {destination}')
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.runtime-import-', dir=destination.parent) as temporary:
        staging = Path(temporary)
        archive = staging / 'runtime.tgz'
        parsed = urllib.parse.urlparse(str(source))
        if parsed.scheme == 'https':
            with urllib.request.urlopen(str(source), timeout=60) as response, archive.open('wb') as stream:
                shutil.copyfileobj(response, stream)
        elif '://' in str(source):
            raise ValueError('remote runtime artifacts require HTTPS')
        else:
            shutil.copyfile(source, archive)
        if checksum(archive) != expected.lower():
            raise ValueError('runtime artifact SHA256 differs from the supplied checksum')
        output = staging / 'out'
        output.mkdir()
        extract(archive, output)
        verify(output)
        output.rename(destination)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    export = commands.add_parser('export')
    export.add_argument('--runtime-output', type=Path, required=True)
    export.add_argument('--output', type=Path, required=True)
    acquire = commands.add_parser('import')
    acquire.add_argument('--source', required=True)
    acquire.add_argument('--sha256', required=True)
    acquire.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if args.command == 'export':
        print(export_artifact(args.runtime_output, args.output))
    else:
        import_artifact(args.source, args.sha256, args.output)


if __name__ == '__main__':
    main()
