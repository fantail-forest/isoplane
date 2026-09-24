#!/usr/bin/env python3
"""Download and verify a publication manifest and its canonical NuGet packages."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import urllib.request
import xml.etree.ElementTree as ET
import zipfile

ORIGIN = 'https://releases.isoplane.dev'
NATIVE_LIBRARIES = {'linux-x64': 'libisoplane_native.so', 'win-x64': 'isoplane_native.dll', 'osx-arm64': 'libisoplane_native.dylib'}
PLATFORMS = tuple(NATIVE_LIBRARIES)


def require(condition, message):
    if not condition:
        raise ValueError(message)


def package_ids():
    spec = importlib.util.spec_from_file_location('preview', Path(__file__).with_name('build-preview.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return tuple(module.PROJECTS)


def digest(value):
    require(isinstance(value, str) and re.fullmatch('[a-f0-9]{64}', value), 'invalid SHA256')
    return value


class Downloads:
    """Fetch only from the public release origin, with bounded reads and no redirects."""
    def __init__(self):
        class NoRedirect(urllib.request.HTTPRedirectHandler):
            def redirect_request(self, req, fp, code, msg, headers, newurl):
                return None
        self.opener = urllib.request.build_opener(NoRedirect())
        self.opener.addheaders = [('User-Agent', 'Isoplane-Release-Verifier/0.1')]

    def fetch(self, key, output, limit):
        with self.opener.open(f'{ORIGIN}/{key}', timeout=120) as response:
            size = 0
            while data := response.read(1024 * 1024):
                size += len(data)
                require(size <= limit, f'{key}: exceeds download limit')
                output.write(data)

    def exists(self, key):
        request = urllib.request.Request(f'{ORIGIN}/{key}', method='HEAD')
        with self.opener.open(request, timeout=120) as response:
            require(response.status == 200, f'{key}: unavailable')


def fetch_json(downloads, key, expected, directory):
    path = directory / ('metadata-' + hashlib.sha256(key.encode()).hexdigest() + '.json')
    fetch_checked(downloads, key, expected, path, 1024 * 1024)
    return json.loads(path.read_bytes())


def fetch_checked(downloads, key, expected, path, limit):
    digest(expected)
    with path.open('xb') as stream:
        downloads.fetch(key, stream, limit)
    with path.open('rb') as stream:
        require(hashlib.file_digest(stream, 'sha256').hexdigest() == expected, f'{key}: SHA256 mismatch')


def verify_nuspec(path, package_id, version, revision):
    with zipfile.ZipFile(path) as archive:
        specs = [entry for entry in archive.infolist() if entry.filename.endswith('.nuspec')]
        require(len(specs) == 1 and specs[0].file_size <= 1024 * 1024, 'expected one bounded NuSpec')
        root = ET.fromstring(archive.read(specs[0]))
        for element in root.iter():
            element.tag = element.tag.rsplit('}', 1)[-1]
        require(root.findtext('metadata/id') == package_id and root.findtext('metadata/version') == version,
                f'{path.name}: NuSpec identity mismatch')
        repository = root.find('metadata/repository')
        require(repository is not None and repository.get('commit') == revision, f'{path.name}: source revision mismatch')
        if package_id in ('Isoplane.Cli', 'Isoplane.Native'):
            for rid, name in NATIVE_LIBRARIES.items():
                prefix = f'runtimes/{rid}/native/' if package_id == 'Isoplane.Native' else 'tools/net10.0/any/runtime/'
                require(prefix + name in archive.namelist() and archive.getinfo(prefix + name).file_size > 0,
                        f'{path.name}: missing native asset for {rid}')


def verify(version, manifest_sha256, directory, downloads):
    """Verify all inputs before producing the ordered package list for publication."""
    require(re.fullmatch(r'(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[A-Za-z0-9]+(?:[.-][A-Za-z0-9]+)*)?', version),
            'invalid release version')
    directory.mkdir(parents=True, exist_ok=False)
    manifest = fetch_json(downloads, f'publication/{version}/manifest.json', manifest_sha256, directory)
    require(manifest['version'] == version, 'publication version mismatch')
    revision = manifest['sourceRevision']
    require(isinstance(revision, str) and re.fullmatch('[a-f0-9]{40}', revision), 'invalid source revision')
    require(set(manifest['releases']) == set(PLATFORMS), 'all supported platform releases are required')
    for rid in PLATFORMS:
        prefix = f'isoplane-{rid}-{version}'
        release = fetch_json(downloads, f'{prefix}/release.json', manifest['releases'][rid], directory)
        require(release['sourceRevision'] == revision and release['packageVersion'] == version
                and release['runtimeIdentifier'] == rid, f'{rid}: release identity mismatch')
        require(release['toolchain']['archive'] == 'toolchain.tgz', f'{rid}: unexpected toolchain path')
        digest(release['toolchain']['archiveSha256'])
        downloads.exists(f'{prefix}/toolchain.tgz')
    packages = manifest['packages']
    ids = package_ids()
    require(isinstance(packages, dict) and set(packages) == set(ids), 'canonical package set mismatch')
    ordered = []
    for package_id in sorted(ids, key=lambda value: (value == 'Isoplane.Cli', value)):
        filename = f'{package_id}.{version}.nupkg'
        path = directory / filename
        fetch_checked(downloads, f'nuget/{version}/{filename}', packages[package_id], path, 1024 ** 3)
        verify_nuspec(path, package_id, version, revision)
        ordered.append(str(path.resolve()))
    (directory / 'packages.json').write_text(json.dumps(ordered) + '\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--version', required=True)
    parser.add_argument('--manifest-sha256', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    verify(args.version, args.manifest_sha256, args.output, Downloads())


if __name__ == '__main__':
    main()
