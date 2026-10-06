#!/usr/bin/env python3
"""Show, check or move the declared release version and the in-repo SDK pins that must match it.

    scripts/release-version.py show          print the declared version
    scripts/release-version.py check         fail on any pin that differs from it
    scripts/release-version.py set VERSION   declare VERSION and rewrite every pin
"""
import argparse
import os
from pathlib import Path
import re
import sys
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
DECLARATION = 'Directory.Build.props'
SCANNED_SUFFIXES = ('.csproj', '.props', '.targets')
EXCLUDED_PATHS = frozenset({'vendor', 'tools/debugger-libs', 'guest-runtime/work', 'handoffs', 'target'})
EXCLUDED_DIRECTORIES = frozenset({'bin', 'obj', 'node_modules'})
SEMVER = re.compile(r'^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(-[0-9A-Za-z.-]+)?$')
PIN = re.compile(r'(?P<prefix>\bSdk="(?:Isoplane\.Sdk|Isoplane\.AspNetCore)/|<PackageReference Include="(?:Isoplane\.Sdk|Isoplane\.AspNetCore|Isoplane\.Testing)" Version=")(?P<version>[^"]+)"')


def declared(root=ROOT):
    element = ET.parse(root / DECLARATION).getroot().find('./PropertyGroup/Version')
    if element is None or not element.text or not SEMVER.match(element.text):
        raise ValueError(f'{DECLARATION} must declare one semantic release version')
    return element.text


def project_files(root=ROOT):
    for directory, subdirectories, files in os.walk(root):
        relative = Path(directory).relative_to(root).as_posix()
        subdirectories[:] = sorted(child for child in subdirectories
                                   if child not in EXCLUDED_DIRECTORIES and not child.startswith('.')
                                   and f'{relative}/{child}'.removeprefix('./') not in EXCLUDED_PATHS)
        for name in sorted(files):
            if name.endswith(SCANNED_SUFFIXES):
                yield Path(directory) / name


def pins(root=ROOT):
    for path in project_files(root):
        for match in PIN.finditer(path.read_text()):
            yield path, match.group('version')


def check(root=ROOT):
    version = declared(root)
    return [(path.relative_to(root).as_posix(), pinned) for path, pinned in pins(root) if pinned != version]


def set_version(version, root=ROOT):
    if not SEMVER.match(version):
        raise ValueError(f'not a semantic version: {version}')
    declaration = root / DECLARATION
    text = declaration.read_text()
    updated = re.sub(r'<Version>[^<]+</Version>', f'<Version>{version}</Version>', text, count=1)
    if updated == text and declared(root) != version:
        raise ValueError(f'{DECLARATION} has no <Version> element to rewrite')
    declaration.write_text(updated)
    for path in project_files(root):
        text = path.read_text()
        rewritten = PIN.sub(lambda match: match.group('prefix') + version + '"', text)
        if rewritten != text:
            path.write_text(rewritten)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest='command', required=True)
    commands.add_parser('show')
    commands.add_parser('check')
    commands.add_parser('set').add_argument('version')
    args = parser.parse_args()
    try:
        if args.command == 'show':
            print(declared())
        elif args.command == 'check':
            drift = check()
            for path, pinned in drift:
                print(f'{path}: pins {pinned}, release version is {declared()}', file=sys.stderr)
            if drift:
                raise SystemExit(f'{len(drift)} pin(s) differ from the declared release version; run scripts/release-version.py set')
        else:
            set_version(args.version)
    except (OSError, ValueError, ET.ParseError) as error:
        raise SystemExit(f'release-version: {error}') from error


if __name__ == '__main__':
    main()
