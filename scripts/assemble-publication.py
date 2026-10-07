#!/usr/bin/env python3
"""Describe prepared packages and releases for the publication uploader."""
import argparse
import hashlib
import json
from pathlib import Path
import importlib.util
import zipfile

spec = importlib.util.spec_from_file_location('release_manifest', Path(__file__).with_name('release-manifest.py'))
manifest = importlib.util.module_from_spec(spec)
spec.loader.exec_module(manifest)


def record(path, key, sha256=None):
    actual = manifest.digest(path)
    if sha256 is not None and actual != sha256:
        raise ValueError(f'{path}: content differs from its release manifest')
    return dict(file=str(path.resolve()), key=key, sha256=actual,
                contentType='application/json' if path.suffix == '.json' else
                'application/gzip' if path.suffix == '.tgz' else 'application/octet-stream')


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
            if name in result:
                raise ValueError(f'{path}: duplicate package entry {name}')
            with archive.open(entry) as stream:
                result[name] = hashlib.file_digest(stream, 'sha256').hexdigest()
        return result


def verify_packages(packages, releases, descriptions, version):
    """Require canonical payloads to match the union of verified platform packages."""
    expected = {}
    expected_ids = None
    for release, description in zip(releases, descriptions):
        ids = set()
        for package in description['packages']:
            package_id = package['id']
            if package_id in ids or package['version'] != version:
                raise ValueError(f'{release}: duplicate package or wrong version: {package_id}')
            ids.add(package_id)
            payload = package_payload(release / package['file'])
            combined = expected.setdefault(package_id, {})
            for name, digest in payload.items():
                if name in combined and combined[name] != digest:
                    raise ValueError(f'{package_id}: platform releases disagree on {name}')
                combined[name] = digest
        if expected_ids is not None and ids != expected_ids:
            raise ValueError(f'{release}: platform package sets differ')
        expected_ids = ids
    actual_ids = {p.name.removesuffix(f'.{version}.nupkg') for p in packages}
    if actual_ids != set(expected):
        raise ValueError('Canonical and platform package sets differ')
    for package in packages:
        package_id = package.name.removesuffix(f'.{version}.nupkg')
        if package_payload(package) != expected[package_id]:
            raise ValueError(f'{package}: payload differs from the verified platform releases; rebuild matching releases before publication')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--releases', type=Path, required=True)
    parser.add_argument('--version', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    releases = [args.releases / f'isoplane-{rid}-{args.version}'
                for rid in ('linux-x64', 'win-x64', 'osx-arm64')]
    descriptions = [json.loads((path / 'release.json').read_text()) for path in releases]
    if len({item['sourceRevision'] for item in descriptions}) != 1:
        parser.error('Platform releases must carry the same source revision.')
    operations, metadata = [], []
    publication = dict(version=args.version, sourceRevision=descriptions[0]['sourceRevision'], packages={}, releases={})
    for path, description in zip(releases, descriptions):
        operations.extend(record(path / p['file'], f"{path.name}/{p['file']}", p['sha256']) for p in description['packages'])
        metadata.append(record(path / 'release.json', f'{path.name}/release.json'))
        publication['releases'][description['runtimeIdentifier']] = metadata[-1]['sha256']
    packages = sorted((args.output / 'nuget' / args.version).glob('*.nupkg'))
    verify_packages(packages, releases, descriptions, args.version)
    for package in packages:
        operation = record(package, package.relative_to(args.output).as_posix())
        operations.append(operation)
        publication['packages'][package.name.removesuffix(f'.{args.version}.nupkg')] = operation['sha256']
    output = args.output / 'publication' / args.version / 'manifest.json'
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(publication, indent=2) + '\n')
    operations.extend(metadata)
    operations.append(record(output, output.relative_to(args.output).as_posix()))
    (args.output / 'upload-plan.json').write_text(json.dumps(operations, indent=2) + '\n')
    (args.output / 'packages.json').write_text(json.dumps([str(p.resolve()) for p in packages]) + '\n')


if __name__ == '__main__':
    main()
