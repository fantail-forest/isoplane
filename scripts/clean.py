"""Remove inactive build and test output belonging to this worktree."""

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
                candidates[child] = 'unmanaged development output; inspect before removing'
    excluded = {'.git', 'node_modules', 'target', 'work', 'out', '__pycache__'}
    import os
    for current, directories, _ in os.walk(root, followlinks=False):
        current = Path(current)
        for name in directories:
            if name in {'bin', 'obj', '.isoplane', 'obj-mono'}:
                candidates[current / name] = 'project outputs; .isoplane may include pending deployments'
        directories[:] = [name for name in directories
                          if name not in excluded | {'bin', 'obj', '.isoplane', 'obj-mono', '.isoplane-dev', '.build', '.artifacts', '.test-runs'}
                          and not (current / name).is_symlink()]
    for path, purpose in sorted(candidates.items()):
        if path.exists():
            if path.is_symlink():
                yield path, purpose + f'; shared/external target -> {path.resolve()}', storage.size_bytes(path.resolve())
            else:
                yield path, purpose, storage.size_bytes(path)


def clean_output(path, dry_run):
    reason = storage.source_preservation_reason(path)
    if reason:
        print(f'retained {path}: {reason}')
        removed = storage.clean_source_siblings(path, dry_run)
    else:
        removed = [(path, storage.size_bytes(path))]
        if not dry_run:
            import shutil
            shutil.rmtree(path)
    for child, size in removed:
        print(f'{size:>14,} bytes  {"would remove" if dry_run else "removed"}  {child}')


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
    roots = []
    for name in ('.build', '.test-runs'):
        parent = ROOT / name
        if parent.is_dir() and not parent.is_symlink():
            for path in sorted(parent.iterdir()):
                if path.is_dir() and not path.is_symlink():
                    if path.name == storage.platform_name() or args.all:
                        roots.append(path)
                    else:
                        print(f"retained {path}: use --all to discard output for other platforms or portable targets")
    shared = ROOT / ".artifacts/shared"
    if args.all and shared.is_dir() and not shared.is_symlink():
        roots.append(shared)
    for root in roots:
        if not root.exists():
            continue
        print(f'{storage.size_bytes(root):>14,} bytes  worktree output  {root}')
        if not args.report:
            clean_output(root, args.dry_run)
    local = list(inventory(ROOT, target))
    for path, purpose, size in local:
        removable = False
        if args.all and path.is_relative_to(ROOT) and not path.is_symlink() and path.name in {
                'target', 'out', 'work', 'bin', 'obj', 'obj-mono'}:
            ignored = subprocess.run(['git', 'check-ignore', '-q', '--', path.name], cwd=path.parent)
            if ignored.returncode not in (0, 1):
                raise RuntimeError(f'Cannot determine whether output is ignored: {path}')
            removable = ignored.returncode == 0
        if removable and not args.report:
            clean_output(path, args.dry_run)
        else:
            print(f'{size:>14,} bytes  retained  {path} ({purpose})')
    print('Sizes are logical file bytes; shared data may be counted more than once.')
    print('Stop builds and tests before cleanup. Use --all for shared and legacy build outputs.')


if __name__ == '__main__':
    main()
