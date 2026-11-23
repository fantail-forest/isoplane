#!/usr/bin/env python3
"""Verify the combined NuGet feed against the three prepared platform releases."""
import argparse
import hashlib
import json
from pathlib import Path
import zipfile

from release_packages import SHARED_PACKAGES, digest, verify_nuspec


def package_payload(path):
    """Hash package contents, excluding NuGet container metadata."""
    with zipfile.ZipFile(path) as archive:
        result = {}
        for entry in archive.infolist():
            name = entry.filename
            if (entry.is_dir() or name.endswith('.nuspec') or name in
                    ('[Content_Types].xml', '.signature.p7s') or
                    name.startswith(('_rels/', 'package/'))):
                continue
            with archive.open(entry) as stream:
                result[name] = hashlib.file_digest(stream, 'sha256').hexdigest()
        return result


def verify_packages(packages, releases, descriptions, version):
    """Require canonical payloads to match the union of verified platform packages."""
    expected = {}
    expected_ids = {*SHARED_PACKAGES, 'Isoplane.Native', 'Isoplane.Cli'}
    for release, description in zip(releases, descriptions):
        for package in description['packages']:
            package_id = package['id']
            path = release / package['file']
            if digest(path) != package['sha256']:
                raise ValueError(f'{path}: content differs from its release manifest')
            payload = package_payload(path)
            combined = expected.setdefault(package_id, {})
            for name, entry_hash in payload.items():
                if name in combined and combined[name] != entry_hash:
                    raise ValueError(f'{package_id}: platform releases disagree on {name}')
                combined[name] = entry_hash
        if {package['id'] for package in description['packages']} != expected_ids:
            raise ValueError(f'{release}: unexpected platform package set')
    actual_ids = {p.name.removesuffix(f'.{version}.nupkg') for p in packages}
    if actual_ids != set(expected):
        raise ValueError('Canonical and unexpected platform package set')
    for package in packages:
        package_id = package.name.removesuffix(f'.{version}.nupkg')
        verify_nuspec(package, package_id, version, descriptions[0]['sourceRevision'])
        if package_payload(package) != expected[package_id]:
            raise ValueError(f'{package}: payload differs from the verified platform releases; rebuild matching releases before publication')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--releases', type=Path, required=True)
    parser.add_argument('--version', required=True)
    parser.add_argument('--feed', type=Path, required=True)
    args = parser.parse_args()
    releases = [args.releases / f'isoplane-{rid}-{args.version}'
                for rid in ('linux-x64', 'win-x64', 'osx-arm64')]
    descriptions = [json.loads((path / 'release.json').read_text()) for path in releases]
    if len({item['sourceRevision'] for item in descriptions}) != 1:
        parser.error('Platform releases must carry the same source revision.')
    packages = sorted(args.feed.glob('*.nupkg'))
    verify_packages(packages, releases, descriptions, args.version)


if __name__ == '__main__':
    main()
