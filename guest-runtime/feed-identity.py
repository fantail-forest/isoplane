"""Identify runtime inputs and validate a published runtime feed before use."""
import argparse
import base64
import hashlib
import json
from pathlib import Path
import subprocess

PACKAGES = (
    ('Microsoft.NETCore.App.Runtime.Mono.wasi-wasm', 'IsoplaneGuestRuntimePack'),
    ('Microsoft.NET.Runtime.WebAssembly.Wasi.Sdk', 'IsoplaneGuestWasiSdkPack'),
)


def file_digest(stream, algorithm):
    digest = hashlib.new(algorithm)
    for chunk in iter(lambda: stream.read(1024 * 1024), b''):
        digest.update(chunk)
    return digest.digest()


def evaluate(props, names):
    evaluation = subprocess.run(
        ['dotnet', 'msbuild', str(props), '-nologo', '-getProperty:' + ','.join(names)],
        capture_output=True, text=True)
    if evaluation.returncode != 0:
        raise ValueError(f'MSBuild could not evaluate {props}:\n{evaluation.stdout}{evaluation.stderr}')
    return json.loads(evaluation.stdout)['Properties']


def verify_feed(feed, expected_sources=None):
    feed = Path(feed)
    expected_sources = expected_sources or {}
    names = [*expected_sources, 'IsoplaneGuestWasiSdkPackDirectory']
    names += [prefix + suffix for _, prefix in PACKAGES for suffix in ('Version', 'Sha512')]
    properties = evaluate(feed / 'Isoplane.GuestRuntime.props', names)

    def field(name):
        value = properties[name]
        if not value:
            raise ValueError(f'{feed} has no {name}; rebuild the runtime feed')
        return value

    for name, expected in expected_sources.items():
        actual = field(name)
        if actual != expected:
            raise ValueError(f'{name}: expected {expected}, received {actual}')

    for package, prefix in PACKAGES:
        version = field(prefix + 'Version')
        if Path(version).name != version or '\\' in version:
            raise ValueError(f'invalid runtime package version: {version}')
        path = feed / f'{package}.{version}.nupkg'
        with path.open('rb') as stream:
            actual = base64.b64encode(file_digest(stream, 'sha512')).decode()
        if actual != field(prefix + 'Sha512'):
            raise ValueError(f'runtime package content differs from published identity: {path}')
    directory = field('IsoplaneGuestWasiSdkPackDirectory')
    if not Path(directory).is_dir():
        raise ValueError(f'unpacked WASI SDK package is missing: {directory}')
    return field('IsoplaneGuestRuntimePackVersion') + '@sha512:' + field('IsoplaneGuestRuntimePackSha512')


def release_sources(wasi_sdk_version):
    repository = Path(__file__).resolve().parent.parent

    def revision(path):
        return subprocess.check_output(
            ['git', '-C', str(repository), 'rev-parse', f'HEAD:vendor/{path}'],
            text=True).strip()

    runtime = revision('dotnet-runtime')
    return {
        'IsoplaneGuestRuntimeSourceRevision': runtime,
        'IsoplaneGuestRuntimeBuildInputs': runtime,
        'IsoplaneGuestOpenSslSourceRevision': revision('openssl'),
        'IsoplaneGuestIcuSourceRevision': revision('icu'),
        'IsoplaneGuestWasiSdkVersion': wasi_sdk_version,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='action', required=True)
    artifact = commands.add_parser('verify-artifact')
    artifact.add_argument('feed', type=Path)
    release = commands.add_parser('verify-release', help='Check the feed against committed dependency pins')
    release.add_argument('feed', type=Path)
    release.add_argument('wasi_sdk_version')
    args = parser.parse_args()
    try:
        expected = release_sources(args.wasi_sdk_version) if args.action == 'verify-release' else None
        print(verify_feed(args.feed, expected))
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f'{error}\n')


if __name__ == '__main__':
    main()
