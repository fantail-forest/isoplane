#!/usr/bin/env python3
"""Assemble a common NuGet feed from completed Linux, Windows and ARM64 Mac releases."""
import argparse
import hashlib
import importlib.util
import json
import re
from pathlib import Path
import shutil
import tempfile
import xml.etree.ElementTree as ET
import zipfile


def load(name):
    spec = importlib.util.spec_from_file_location(name.replace('-', '_'), Path(__file__).with_name(name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


preview = load('build-preview')
publication = load('verify-publication')


def native_name(rid):
    return publication.NATIVE_LIBRARIES[rid]


def verify_release(directory):
    manifest = json.loads((directory / 'release.json').read_text())
    rid, version, revision = (manifest[key] for key in ('runtimeIdentifier', 'packageVersion', 'sourceRevision'))
    publication.require(isinstance(version, str) and re.fullmatch(r'(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[A-Za-z0-9]+(?:[.-][A-Za-z0-9]+)*)?', version), 'invalid release version')
    publication.require(isinstance(revision, str) and re.fullmatch('[a-f0-9]{40}', revision), 'invalid source revision')
    publication.require(rid in publication.PLATFORMS, 'unsupported release platform')
    publication.require(directory.name == f'isoplane-{rid}-{version}', 'release directory identity mismatch')
    records = manifest['packages']
    publication.require(len(records) == len(preview.PROJECTS) and {p['id'] for p in records} == set(preview.PROJECTS),
                        'release package inventory mismatch')
    for record in records:
        expected = f"packages/{record['id']}.{version}.nupkg"
        publication.require(record['file'] == expected and record['version'] == version, 'invalid package path or version')
        publication.require(preview.sha(directory / expected) == publication.digest(record['sha256']), 'release package SHA256 mismatch')
    toolchain = manifest['toolchain']
    publication.require(toolchain['archive'] == 'toolchain.tgz', 'unexpected toolchain archive path')
    publication.require(preview.sha(directory / 'toolchain.tgz') == publication.digest(toolchain['archiveSha256']),
                        'toolchain SHA256 mismatch')
    library_name = native_name(rid)
    with zipfile.ZipFile(directory / 'packages' / f'Isoplane.Native.{version}.nupkg') as package:
        library = package.read(f'runtimes/{rid}/native/{library_name}')
    with load('build_storage').records() as temporary:
        path = Path(temporary) / library_name
        path.write_bytes(library)
        preview.verify_packages(directory / 'packages', version, revision, path, rid)
    return manifest, library


def combine_package(original, destination, additions):
    """Keep canonical managed bytes and add verified platform-native assets before signing."""
    with zipfile.ZipFile(original) as source, zipfile.ZipFile(destination, 'w', zipfile.ZIP_DEFLATED) as output:
        entries = source.infolist()
        names = [entry.filename for entry in entries]
        publication.require(len(names) == len(set(names)), 'duplicate ZIP entries')
        publication.require('.signature.p7s' not in names, 'assemble packages before signing')
        types = ET.fromstring(source.read('[Content_Types].xml'))
        namespace = '{http://schemas.openxmlformats.org/package/2006/content-types}'
        extensions = {entry.get('Extension') for entry in types}
        for extension in sorted({Path(name).suffix[1:] for name in additions} - extensions):
            ET.SubElement(types, namespace + 'Default', Extension=extension, ContentType='application/octet')
        contents = dict(additions)
        for entry in entries:
            if entry.filename == '[Content_Types].xml':
                contents[entry.filename] = ET.tostring(types, encoding='utf-8', xml_declaration=True)
            elif entry.filename in additions:
                publication.require(source.read(entry) == additions[entry.filename], 'canonical native asset differs')
            else:
                contents[entry.filename] = source.read(entry)
        for name, data in sorted(contents.items()):
            info = zipfile.ZipInfo(name, (1980, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = 0o100644 << 16
            output.writestr(info, data)


def assemble(directories, output):
    """Verify immutable inputs and atomically expose canonical packages plus their upload plan."""
    publication.require(not output.exists(), 'publication output already exists')
    releases, libraries = {}, {}
    for directory in directories:
        manifest, library = verify_release(directory)
        rid = manifest['runtimeIdentifier']
        publication.require(rid not in releases, 'duplicate platform release')
        releases[rid] = (directory, manifest)
        libraries[rid] = library
    publication.require(set(releases) == set(publication.PLATFORMS), 'all supported platform releases are required')
    version, revision = releases['linux-x64'][1]['packageVersion'], releases['linux-x64'][1]['sourceRevision']
    publication.require(all(m['packageVersion'] == version and m['sourceRevision'] == revision for _, m in releases.values()),
                        'platform releases must have the same version and source revision')
    canonical = releases['linux-x64'][0] / 'packages'
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.publication-', dir=output.parent) as temporary:
        stage = Path(temporary) / 'publication'
        feed = stage / 'nuget' / version
        feed.mkdir(parents=True)
        manifest = {'version': version, 'sourceRevision': revision, 'packages': {}, 'releases': {}}
        for package_id in preview.PROJECTS:
            filename = f'{package_id}.{version}.nupkg'
            if package_id in ('Isoplane.Native', 'Isoplane.Cli'):
                additions = {}
                for rid in publication.PLATFORMS:
                    key = (f'runtimes/{rid}/native/' if package_id == 'Isoplane.Native' else 'tools/net10.0/any/runtime/') + native_name(rid)
                    additions[key] = libraries[rid]
                combine_package(canonical / filename, feed / filename, additions)
            else:
                shutil.copyfile(canonical / filename, feed / filename)
            manifest['packages'][package_id] = preview.sha(feed / filename)
        metadata = stage / 'publication' / version / 'manifest.json'
        metadata.parent.mkdir(parents=True)
        operations, release_manifests = [], []
        for rid in publication.PLATFORMS:
            directory, release = releases[rid]
            operations.append(record(directory / 'toolchain.tgz', f'{directory.name}/toolchain.tgz'))
            for package in release['packages']:
                operations.append(record(directory / package['file'], f"{directory.name}/{package['file']}"))
            release_manifests.append(record(directory / 'release.json', f'{directory.name}/release.json'))
            manifest['releases'][rid] = preview.sha(directory / 'release.json')
        metadata.write_text(json.dumps(manifest, indent=2) + '\n')
        for package in sorted(feed.iterdir()):
            relative = package.relative_to(stage)
            operations.append(record(package, relative.as_posix(), output / relative))
        operations.extend(release_manifests)
        operations.append(record(metadata, metadata.relative_to(stage).as_posix(), output / metadata.relative_to(stage)))
        (stage / 'upload-plan.json').write_text(json.dumps(operations, indent=2) + '\n')
        stage.rename(output)
    return metadata.relative_to(stage)


def record(path, key, final_path=None):
    return {'file': str((final_path or path).resolve()), 'key': key, 'sha256': preview.sha(path),
            'contentType': 'application/json' if key.endswith('.json') else 'application/gzip' if key.endswith('.tgz') else 'application/octet-stream'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--release', type=Path, action='append', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    manifest = assemble([path.resolve() for path in args.release], args.output.resolve())
    print(f'Publication manifest SHA256: {preview.sha(args.output / manifest)}')


if __name__ == '__main__':
    main()
