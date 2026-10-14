#!/usr/bin/env python3
"""Write a license text for each package in a nuget-license JSON inventory.

Texts come from the restored package where it carries one, otherwise from a
download cache that is filled only for texts it does not hold yet.
"""
import argparse
import hashlib
import json
import re
import shutil
import subprocess
import sys
import urllib.request
import xml.etree.ElementTree as ElementTree
from pathlib import Path

# A tagged release of the SPDX license list, so a cached text never goes stale.
SPDX_TEXT_URL = 'https://raw.githubusercontent.com/spdx/license-list-data/v3.29.0/text/{}.txt'

# nuget-license's LicenseInformationOrigin values, as its JSON output numbers them.
EXPRESSION, URL, OVERWRITE, FILE = 0, 1, 4, 5


class LicenseTextError(Exception):
    pass


def global_packages_folder(dotnet):
    listing = subprocess.run([dotnet, 'nuget', 'locals', 'global-packages', '--list', '--force-english-output'],
                             check=True, capture_output=True, text=True).stdout
    match = re.search(r'^global-packages: (.+)$', listing, re.MULTILINE)
    if match is None:
        raise LicenseTextError(f'dotnet nuget locals printed no global-packages folder: {listing!r}')
    return Path(match.group(1).strip())


def license_ids(expression):
    """The license and exception identifiers in an SPDX expression."""
    return [token for token in re.split(r'[\s()]+', expression) if token and token not in ('AND', 'OR', 'WITH')]


def cached_download(url, cache):
    key = hashlib.sha256(url.encode()).hexdigest()
    for existing in cache.glob(f'{key}.*'):
        if existing.suffix in ('.txt', '.html'):
            return existing
    try:
        with urllib.request.urlopen(url, timeout=60) as response:
            content = response.read()
            extension = '.txt' if response.headers.get_content_type() == 'text/plain' else '.html'
    except OSError as error:
        raise LicenseTextError(f'downloading {url}: {error}') from error
    cache.mkdir(parents=True, exist_ok=True)
    destination = cache / f'{key}{extension}'
    partial = destination.with_suffix('.partial')
    partial.write_bytes(content)
    partial.replace(destination)
    return destination


class Package:
    def __init__(self, entry, packages):
        self.id = entry['PackageId']
        self.version = entry['PackageVersion']
        self.license = entry['License']
        self.license_url = entry.get('LicenseUrl')
        self.origin = entry['LicenseInformationOrigin']
        self.directory = packages / self.id.lower() / self.version.lower()
        if not self.directory.is_dir():
            raise LicenseTextError(f'{self}: not restored in {packages}; run task managed:build')

    def __str__(self):
        return f'{self.id} {self.version}'

    def declared_license_file(self):
        nuspec = ElementTree.parse(self.directory / f'{self.id.lower()}.nuspec')
        for element in nuspec.iter():
            if element.tag.rpartition('}')[2] == 'license' and element.get('type') == 'file':
                return self.directory / element.text.strip()
        raise LicenseTextError(f'{self}: nuget-license reported a license file the nuspec does not declare')

    def shipped_license_file(self):
        """The one root-level license file a package without license metadata ships, if it ships exactly one."""
        candidates = [path for path in self.directory.iterdir()
                      if path.is_file() and 'license' in path.name.lower() and path.suffix.lower() in ('', '.txt', '.md')]
        return candidates[0] if len(candidates) == 1 else None

    def spdx_text(self, cache):
        texts = [cached_download(SPDX_TEXT_URL.format(identifier), cache).read_bytes()
                 for identifier in license_ids(self.license)]
        return b'\n\n'.join(texts), '.txt'

    def license_text(self, cache):
        """The license text and the file extension it is published under."""
        if self.origin in (EXPRESSION, OVERWRITE):
            return self.spdx_text(cache)
        if self.origin == FILE:
            path = self.declared_license_file()
            return path.read_bytes(), path.suffix.lower() or '.txt'
        if self.origin == URL:
            shipped = self.shipped_license_file()
            if shipped is not None:
                return shipped.read_bytes(), shipped.suffix.lower() or '.txt'
            if self.license != self.license_url:
                return self.spdx_text(cache)
            path = cached_download(self.license_url, cache)
            return path.read_bytes(), path.suffix
        raise LicenseTextError(f'{self}: nuget-license found no license (origin {self.origin})')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--inventory', type=Path, required=True, help='nuget-license JsonPretty output')
    parser.add_argument('--output', type=Path, required=True, help='directory to replace with one text per package')
    parser.add_argument('--cache', type=Path, required=True, help='downloaded texts, kept between runs')
    parser.add_argument('--dotnet', required=True, help='the dotnet executable that restored the packages')
    args = parser.parse_args()
    packages = global_packages_folder(args.dotnet)
    texts = {}
    failures = []
    for entry in json.loads(args.inventory.read_text()):
        try:
            package = Package(entry, packages)
            texts[f'{package.id}__{package.version}'] = package.license_text(args.cache)
        except (LicenseTextError, OSError, ElementTree.ParseError) as error:
            failures.append(f'{entry["PackageId"]} {entry["PackageVersion"]}: {error}')
    if failures:
        sys.exit('No license text for:\n  ' + '\n  '.join(failures))
    if args.output.exists():
        shutil.rmtree(args.output)
    args.output.mkdir(parents=True)
    for name, (content, extension) in texts.items():
        (args.output / f'{name}{extension}').write_bytes(content)


if __name__ == '__main__':
    main()
