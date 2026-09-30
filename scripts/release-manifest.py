#!/usr/bin/env python3
"""Write the artifact identities consumed by Isoplane setup."""

import argparse
import hashlib
import json
import tarfile
from pathlib import Path


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('kind', choices=('toolchain', 'workload', 'msi', 'release'))
    parser.add_argument('directory', type=Path)
    parser.add_argument('--platform', required=True)
    parser.add_argument('--version', required=True)
    parser.add_argument('--revision')
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    if args.kind == 'release':
        with tarfile.open(args.directory / 'toolchain.tgz') as archive:
            manifest_sha256 = hashlib.sha256(archive.extractfile('./toolchain.json').read()).hexdigest()
        packages = args.directory / 'packages'
        document = dict(sourceRevision=args.revision, packageVersion=args.version,
                        runtimeIdentifier=args.platform,
                        toolchain=dict(archive='toolchain.tgz',
                                       archiveSha256=digest(args.directory / 'toolchain.tgz'),
                                       manifestSha256=manifest_sha256),
                        packages=[dict(id=p.name.removesuffix(f'.{args.version}.nupkg'),
                                       version=args.version, file=f'packages/{p.name}', sha256=digest(p))
                                  for p in sorted(packages.glob(f'*.{args.version}.nupkg'))])
        name = 'release.json'
    else:
        document = dict(runtimeIdentifier=args.platform,
                        dotnetSdkVersion=json.loads((root / 'global.json').read_text())['sdk']['version'],
                        kind=args.kind,
                        files=[dict(path=p.relative_to(args.directory).as_posix(), sha256=digest(p),
                                    executable=bool(p.stat().st_mode & 0o100))
                               for p in sorted(args.directory.rglob('*'))
                               if p.is_file() and p.name != 'toolchain.json'])
        name = 'toolchain.json'
    (args.directory / name).write_text(json.dumps(document, indent=2) + '\n')


if __name__ == '__main__':
    main()
