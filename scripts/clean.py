"""Reclaim leased build caches and inventory other generated output without deleting unowned files."""

import argparse
import json
from pathlib import Path
import subprocess

import build_storage as storage

ROOT = Path(__file__).resolve().parents[1]


def inventory(root, target):
    candidates = {
        target: 'Cargo and fixture outputs',
        root / 'guests/target': 'guest Cargo outputs',
        root / 'guests/out': 'guest fixtures',
        root / 'guest-runtime/out': 'runtime products',
        root / 'guest-runtime/work': 'runtime intermediates',
        root / '.isoplane-dev': 'local tools and legacy release builds',
    }
    development = root / '.isoplane-dev'
    if development.is_dir() and not development.is_symlink():
        for child in development.iterdir():
            if child.is_dir() and not child.is_symlink():
                candidates[child] = 'unleased development output; inspect before removing'
    excluded = {'.git', 'node_modules', 'target', 'work', 'out', '__pycache__'}
    import os
    for current, directories, _ in os.walk(root, followlinks=False):
        current = Path(current)
        for name in directories:
            if name in {'bin', 'obj', '.isoplane', 'obj-mono'}:
                candidates[current / name] = 'project outputs; .isoplane may include pending deployments'
        directories[:] = [name for name in directories
                          if name not in excluded | {'bin', 'obj', '.isoplane', 'obj-mono', '.isoplane-dev'}
                          and not (current / name).is_symlink()]
    for path, purpose in sorted(candidates.items()):
        if path.exists():
            if path.is_symlink():
                yield path, purpose + f'; shared/external target -> {path.resolve()}', storage.size_bytes(path.resolve())
            else:
                yield path, purpose, storage.size_bytes(path)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--all', action='store_true')
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument('--dry-run', action='store_true')
    mode.add_argument('--report', action='store_true')
    args = parser.parse_args()
    metadata = json.loads(subprocess.check_output(
        ['cargo', 'metadata', '--no-deps', '--format-version', '1'], cwd=ROOT))
    target = Path(metadata['target_directory'])
    for root, budget, age in [(storage.shared_root(), storage.PRODUCT_BYTES, None),
                              (storage.diagnostics_root(), storage.LOG_BYTES, storage.LOG_AGE_SECONDS)]:
        if not root.exists():
            continue
        print(f'{storage.size_bytes(root):>14,} bytes  managed  {root}')
        with storage.catalog(root):
            for entry in sorted(root.iterdir()):
                marker = entry / '.lease'
                if entry.is_symlink() or not entry.is_dir() or marker.is_symlink() or not marker.is_file():
                    continue
                with (entry / '.lease').open('r+b') as lease:
                    state = 'inactive' if storage.lock(lease, blocking=False) else 'active'
                owner = entry / 'owner.json'
                try:
                    details = json.loads(owner.read_text()) if owner.is_file() and not owner.is_symlink() else {}
                except (OSError, json.JSONDecodeError) as error:
                    print(f'  ownership record unavailable at {owner}: {error}')
                    details = {}
                label = details.get('identity', 'diagnostic run')
                workspace = details.get('workspace', 'unrecorded workspace')
                print(f'  {state}: {entry.name}  {label}  last producer: {workspace}')
        if not args.report:
            removed = storage.prune(root, 0 if args.all else budget,
                                    age, dry_run=args.dry_run)
            for path, size in removed:
                print(f'{size:>14,} bytes  {"would remove" if args.dry_run else "removed"}  {path}')
    local = list(inventory(ROOT, target))
    compiled = [target / name for name in ('compiled-tests', 'compiled-test-guests', 'compiled-test-packages')]
    compiled.extend(path / 'compiled' for path, _, _ in local if path.name == '.isoplane')
    for root in compiled:
        if root.exists() and args.all and not args.report:
            for path, size in storage.prune(root, 0, dry_run=args.dry_run):
                print(f'{size:>14,} bytes  {"would remove" if args.dry_run else "removed"}  {path}')
    for path, purpose, size in local:
        print(f'{size:>14,} bytes  retained  {path} ({purpose})')
    print('Sizes are logical file bytes; shared data may be counted more than once.')
    print('Active leases are protected. Unleased output and pending deployment archives are retained.')


if __name__ == '__main__':
    main()
