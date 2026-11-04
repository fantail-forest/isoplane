#!/usr/bin/env python3
"""Record and validate the identities of prepared release artifacts."""
import argparse
import hashlib
import json
from pathlib import Path
import runpy
import xml.etree.ElementTree as ET
import zipfile


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def shared_files(packages, managed, notices, package_ids, version):
    """Require the shared packages and support files used by platform packaging."""
    cli = managed / 'publish/Isoplane.Cli/release'
    required = [cli / name for name in (
        'Isoplane.Cli.dll', 'Isoplane.Cli.deps.json', 'Isoplane.Cli.runtimeconfig.json',
        'hotreload/Isoplane.HotReload.dll', 'hotreload/Isoplane.HotReload.deps.json',
        'hotreload/Isoplane.HotReload.runtimeconfig.json')]
    required += [managed / 'bin/Isoplane.Native/release/Isoplane.Native.dll',
                 managed / 'obj/Isoplane.Cli/release/DotnetToolSettings.xml']
    required += [notices / name for name in (
        'rust-dependencies.json', 'managed-dependencies.json',
        'trust/NOTICE', 'trust/ca-bundle.manifest.json', 'sqlite/NOTICE')]
    required += [packages / f'{name}.{version}.nupkg' for name in package_ids]
    for path in required:
        if not path.is_file() or path.stat().st_size == 0:
            raise ValueError(f'Missing prepared artifact: {path}')
    for component in ('dotnet-runtime', 'icu', 'openssl'):
        if not any(path.is_file() for path in (notices / component).rglob('*')):
            raise ValueError(f'Missing prepared notices: {notices / component}')
    return set(required) | {path for directory in (cli, notices)
                            for path in directory.rglob('*') if path.is_file()}


def shared(directory, packages, managed, notices, version, revision, write):
    """Bind the complete portable artifact set to its Linux preparation."""
    verifier = runpy.run_path(str(Path(__file__).with_name('verify-publication.py')))
    package_ids = [name for name in verifier['package_ids']()
                   if name not in ('Isoplane.Native', 'Isoplane.Cli')]
    files = shared_files(packages, managed, notices, package_ids, version)
    expected_packages = {packages / f'{name}.{version}.nupkg' for name in package_ids}
    if set(packages.glob(f'Isoplane.*.{version}.nupkg')) != expected_packages:
        raise ValueError('Prepared shared packages differ from the canonical package set.')
    for name in package_ids:
        verifier['verify_nuspec'](packages / f'{name}.{version}.nupkg', name, version, revision)
    prepared = dict(sourceRevision=revision, packageVersion=version,
                    files={path.relative_to(directory).as_posix(): digest(path) for path in sorted(files)})
    receipt = managed.parent / 'release-shared.json'
    if write:
        receipt.write_text(json.dumps(prepared, indent=2) + '\n')
    elif json.loads(receipt.read_text()) != prepared:
        raise ValueError('Prepared shared artifacts differ from their Linux preparation receipt.')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='kind', required=True)
    release = commands.add_parser('release', help='Record a staged platform release.')
    release.add_argument('--platform', required=True)
    shared_parser = commands.add_parser('shared', help='Validate copied shared artifacts or record their Linux preparation.')
    for name in ('packages', 'managed', 'notices'):
        shared_parser.add_argument(f'--{name}', required=True, type=Path)
    shared_parser.add_argument('--write', action='store_true')
    for command in (release, shared_parser):
        command.add_argument('directory', type=Path)
        command.add_argument('--version', required=True)
        command.add_argument('--revision', required=True)
    args = parser.parse_args()
    if args.kind == 'shared':
        try:
            shared(args.directory, args.packages, args.managed, args.notices, args.version, args.revision, args.write)
        except (OSError, ValueError, zipfile.BadZipFile, ET.ParseError) as error:
            parser.exit(1, f'{error}\nRun task release:shared on Linux and copy the shared artifacts and release-shared.json before packaging this platform.\n')
        return
    packages = args.directory / 'packages'
    document = dict(sourceRevision=args.revision, packageVersion=args.version,
                    runtimeIdentifier=args.platform,
                    packages=[dict(id=p.name.removesuffix(f'.{args.version}.nupkg'),
                                   version=args.version, file=f'packages/{p.name}', sha256=digest(p))
                              for p in sorted(packages.glob(f'*.{args.version}.nupkg'))])
    (args.directory / 'release.json').write_text(json.dumps(document, indent=2) + '\n')


if __name__ == '__main__':
    main()
