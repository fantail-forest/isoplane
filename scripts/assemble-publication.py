#!/usr/bin/env python3
"""Describe prepared packages and releases for the publication uploader."""
import argparse
import json
from pathlib import Path
import importlib.util

spec = importlib.util.spec_from_file_location('release_manifest', Path(__file__).with_name('release-manifest.py'))
manifest = importlib.util.module_from_spec(spec)
spec.loader.exec_module(manifest)


def record(path, key, sha256=None):
    return dict(file=str(path.resolve()), key=key, sha256=sha256 or manifest.digest(path),
                contentType='application/json' if path.suffix == '.json' else
                'application/gzip' if path.suffix == '.tgz' else 'application/octet-stream')


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
        operations.append(record(path / 'toolchain.tgz', f'{path.name}/toolchain.tgz', description['toolchain']['archiveSha256']))
        operations.extend(record(path / p['file'], f"{path.name}/{p['file']}", p['sha256']) for p in description['packages'])
        metadata.append(record(path / 'release.json', f'{path.name}/release.json'))
        publication['releases'][description['runtimeIdentifier']] = metadata[-1]['sha256']
    packages = sorted((args.output / 'nuget' / args.version).glob('*.nupkg'))
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
