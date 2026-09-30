"""Identify runtime inputs and validate a published runtime feed before use."""
import argparse
import base64
import hashlib
from pathlib import Path
import xml.etree.ElementTree as ET
import zipfile


def file_digest(stream, algorithm):
    digest = hashlib.new(algorithm)
    for chunk in iter(lambda: stream.read(1024 * 1024), b''):
        digest.update(chunk)
    return digest.digest()


def verify_feed(feed):
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
        if not Path(directory).is_dir():
            raise ValueError(f'unpacked runtime package is missing: {directory}')
    return field('IsoplaneGuestRuntimePackVersion') + '@sha512:' + field('IsoplaneGuestRuntimePackSha512')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='action', required=True)
    artifact = commands.add_parser('verify-artifact')
    artifact.add_argument('feed', type=Path)
    args = parser.parse_args()
    try:
        print(verify_feed(args.feed))
    except (OSError, ValueError, ET.ParseError, zipfile.BadZipFile) as error:
        parser.exit(1, f'{error}\n')


if __name__ == '__main__':
    main()
