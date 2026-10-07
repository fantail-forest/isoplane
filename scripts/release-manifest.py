#!/usr/bin/env python3
"""Write the package identities consumed by publication and offline setup."""
import argparse
import hashlib
import json
from pathlib import Path


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('kind', choices=('release',))
    parser.add_argument('directory', type=Path)
    parser.add_argument('--platform', required=True)
    parser.add_argument('--version', required=True)
    parser.add_argument('--revision', required=True)
    args = parser.parse_args()
    packages = args.directory / 'packages'
    document = dict(sourceRevision=args.revision, packageVersion=args.version,
                    runtimeIdentifier=args.platform,
                    packages=[dict(id=p.name.removesuffix(f'.{args.version}.nupkg'),
                                   version=args.version, file=f'packages/{p.name}', sha256=digest(p))
                              for p in sorted(packages.glob(f'*.{args.version}.nupkg'))])
    (args.directory / 'release.json').write_text(json.dumps(document, indent=2) + '\n')


if __name__ == '__main__':
    main()
