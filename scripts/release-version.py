#!/usr/bin/env python3
"""Show or move the declared release version and the in-repo SDK pins that must match it.

    scripts/release-version.py show          print the declared version
    scripts/release-version.py set VERSION   declare VERSION and rewrite every pin
    scripts/release-version.py validate VERSION   check a supplied release identity
"""
import argparse
import os
from pathlib import Path
import re
import subprocess

from release_version import validate_release_version

ROOT = Path(__file__).resolve().parents[1]
DECLARATION = 'Directory.Build.props'
SCANNED_SUFFIXES = ('.csproj', '.props', '.targets')
EXCLUDED_PATHS = frozenset({'vendor', 'tools/debugger-libs', 'guest-runtime/work', 'handoffs', 'target'})
EXCLUDED_DIRECTORIES = frozenset({'bin', 'obj', 'node_modules'})
PIN = re.compile(r'(?P<prefix>\bSdk="(?:Isoplane\.Sdk|Isoplane\.AspNetCore)/|<PackageReference Include="(?:Isoplane\.Sdk|Isoplane\.AspNetCore|Isoplane\.AspNetCore\.DataProtection\.EntityFrameworkCore|Isoplane\.OpenTelemetry|Isoplane\.Testing)" Version=")(?P<version>(?!\$\()[^"]+)"')


def declared(root=ROOT):
    evaluation = subprocess.run(
        ['dotnet', 'msbuild', str(root / DECLARATION), '-nologo', '-getProperty:Version'],
        capture_output=True, text=True)
    if evaluation.returncode != 0:
        raise ValueError(f'MSBuild could not evaluate {DECLARATION}:\n{evaluation.stdout}{evaluation.stderr}')
    return validate_release_version(evaluation.stdout.strip())


def project_files(root=ROOT):
    for directory, subdirectories, files in os.walk(root):
        relative = Path(directory).relative_to(root).as_posix()
        subdirectories[:] = sorted(child for child in subdirectories
                                   if child not in EXCLUDED_DIRECTORIES and not child.startswith('.')
                                   and f'{relative}/{child}'.removeprefix('./') not in EXCLUDED_PATHS)
        for name in sorted(files):
            if name.endswith(SCANNED_SUFFIXES):
                yield Path(directory) / name


def set_version(version, root=ROOT):
    validate_release_version(version)
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
    commands.add_parser('set').add_argument('version')
    commands.add_parser('validate').add_argument('version')
    args = parser.parse_args()
    try:
        if args.command == 'show':
            print(declared())
        elif args.command == 'set':
            set_version(args.version)
        else:
            validate_release_version(args.version)
    except (OSError, ValueError) as error:
        raise SystemExit(f'release-version: {error}') from error


if __name__ == '__main__':
    main()
