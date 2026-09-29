"""Worktree-owned build output and explicit cleanup."""

from contextlib import contextmanager
import errno
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import urllib.request


def size_bytes(path):
    try:
        metadata = path.lstat()
        if stat.S_ISREG(metadata.st_mode):
            return metadata.st_size
        if stat.S_ISDIR(metadata.st_mode):
            return sum(size_bytes(child) for child in path.iterdir())
        return 0
    except FileNotFoundError:
        # Active producers may finish and remove temporary files during inventory.
        return 0


def source_preservation_reason(directory):
    """Refuse eviction of unknown repositories or generated trees with local state."""
    directory = Path(directory)
    if not directory.exists():
        return None
    def git(tree, *arguments):
        return subprocess.check_output(['git', '--no-optional-locks', '-C', str(tree), *arguments],
                                       stderr=subprocess.PIPE, text=True).strip()

    def scan_error(error):
        raise error

    for current, directories, files in os.walk(directory, followlinks=False, onerror=scan_error):
        if '.git' not in directories + files:
            continue
        tree = Path(current)
        directories[:] = []
        metadata = tree / '.git'
        record = metadata / 'isoplane-source.json'
        if not metadata.is_dir() or metadata.is_symlink() or not record.is_file():
            return f'unrecognized source repository: {tree}'
        try:
            state = json.loads(record.read_text())
            head = state['head']
            if not isinstance(head, str) or git(tree, 'rev-parse', 'HEAD') != head:
                return f'local source commits: {tree}'
            if git(tree, 'status', '--porcelain', '--untracked-files=all', '--ignore-submodules=none'):
                return f'local source edits: {tree}'
            outputs = state.get('generated_directories', [])
            if not isinstance(outputs, list) or not all(isinstance(name, str) and name not in ('', '.', '..')
                                                       and '/' not in name and '\\' not in name for name in outputs):
                return f'unverified source build directories: {tree}'
            ignored = git(tree, 'ls-files', '--others', '--ignored', '--exclude-standard', '--directory', '-z')
            for name in ignored.split('\0'):
                if name and name.split('/', 1)[0] not in outputs:
                    return f'ignored local source input: {tree / name}'
            if (metadata / 'isoplane-source-backups').exists():
                return f'preserved source files: {tree}'
            branch = git(tree, 'symbolic-ref', 'HEAD')
            refs = git(tree, 'for-each-ref', '--format=%(refname)').splitlines()
            generated = state.get('generated_heads', [head])
            if not isinstance(generated, list) or head not in generated or not all(
                    isinstance(commit, str) and len(commit) in (40, 64)
                    and all(character in '0123456789abcdef' for character in commit) for commit in generated):
                return f'unverified generated source history: {tree}'
            if any(ref != branch and not ref.startswith('refs/isoplane/source-backups/') for ref in refs):
                return f'local source references: {tree}'
            if git(tree, 'rev-list', '--all', '--reflog', '--not', *generated):
                return f'preserved source history: {tree}'
        except (OSError, ValueError, KeyError, TypeError, subprocess.CalledProcessError) as error:
            return f'cannot verify disposable source at {tree}: {error}'
    return None


def clean(root, dry_run=False):
    """Explicitly remove owned output; callers must stop its producers first."""
    root = Path(root)
    if not root.exists():
        return []
    removed = []
    for directory in sorted(root.iterdir()):
        owner = directory / 'owner.json'
        if directory.is_symlink() or not directory.is_dir() or not owner.is_file() or owner.is_symlink():
            continue
        reason = source_preservation_reason(directory)
        if reason:
            print(f'Retained {directory}: {reason}', file=sys.stderr)
            removed.extend(clean_source_siblings(directory, dry_run))
            continue
        removed.append((directory, size_bytes(directory)))
        if not dry_run:
            remove_owned_directory(directory)
    return removed


def remove_owned_directory(directory):
    """Keep the ownership record if deleting build contents fails."""
    for path in directory.iterdir():
        if path.name == 'owner.json':
            continue
        if path.is_dir() and not path.is_symlink():
            shutil.rmtree(path)
        else:
            path.unlink(missing_ok=True)
    shutil.rmtree(directory)


def clean_source_siblings(directory, dry_run):
    """Keep edited repositories without retaining unrelated compiler output."""
    if (directory / '.git').exists():
        return []
    removed = []
    for path in sorted(directory.iterdir()):
        if path.name == 'owner.json':
            continue
        if path.is_symlink():
            continue
        if path.is_dir() and source_preservation_reason(path):
            removed.extend(clean_source_siblings(path, dry_run))
            continue
        removed.append((path, size_bytes(path)))
        if not dry_run:
            if path.is_dir():
                shutil.rmtree(path)
            else:
                path.unlink(missing_ok=True)
    return removed


@contextmanager
def retained_run(root, remove_on_success=False):
    """Keep diagnostics until explicit cleanup; optionally remove successful scratch."""
    root = Path(root)
    root.mkdir(parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix="run-", dir=root))
    (directory / 'owner.json').write_text(json.dumps({'pid': os.getpid(), 'platform': platform_name()}))
    yield directory
    if remove_on_success:
        remove_owned_directory(directory)


def tools_root():
    """Locate installed tools shared by worktrees on this host."""
    if os.name == "nt":
        base = Path(os.environ.get("LOCALAPPDATA", Path.home() / "AppData/Local"))
    elif platform.system() == "Darwin":
        base = Path.home() / "Library/Caches"
    else:
        base = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache"))
    return base / "isoplane/build-products"


def repository_root():
    return Path(__file__).resolve().parents[1]


def platform_name():
    """Separate incompatible platform outputs without tying them to a machine."""
    system = {'Linux': 'linux', 'Darwin': 'osx', 'Windows': 'win'}.get(platform.system(), platform.system().lower())
    machine = platform.machine().lower()
    architecture = {'x86_64': 'x64', 'amd64': 'x64', 'aarch64': 'arm64'}.get(machine, machine)
    return f'{system}-{architecture}'


def build_root(root=None):
    return (repository_root() if root is None else Path(root)) / '.build' / platform_name()


def shared_root():
    """Immutable artifacts shared by consumers of this worktree."""
    return repository_root() / '.artifacts' / 'shared'


def diagnostics_root():
    return repository_root() / '.test-runs' / platform_name()


@contextmanager
def records(explicit=None):
    """Remove successful scratch runs and keep failure evidence in the worktree."""
    if explicit is not None:
        directory = Path(explicit).resolve()
        directory.mkdir(parents=True, exist_ok=True)
        yield directory
    else:
        with retained_run(diagnostics_root(), remove_on_success=True) as directory:
            yield directory


@contextmanager
def product(identity, root=None, read_only=False, name=None):
    """Locate build output without coordinating concurrent producers."""
    root = build_root() / 'products' if root is None else Path(root)
    name = hashlib.sha256(identity.encode()).hexdigest() if name is None else name
    if not re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9._-]*", name):
        raise ValueError(f"invalid build directory name: {name}")
    directory = root / name
    owner = directory / 'owner.json'
    if directory.is_symlink() or owner.is_symlink():
        raise ValueError(f"product storage must not be a symlink: {directory}")
    if read_only:
        yield directory if owner.is_file() else None
        return
    if directory.exists() and not owner.is_file():
        raise ValueError(f"product storage is not owned: {directory}")
    directory.mkdir(parents=True, exist_ok=True)
    if not owner.exists():
        owner.write_text(json.dumps({'identity': identity}))
    yield directory


@contextmanager
def development(root, inherited):
    """Keep a host's mutable build and temporary files inside its worktree."""
    root = Path(root).resolve()
    with product('development', root=build_root(root), name='development') as directory:
        environment = dict(inherited)
        previous = inherited.get('ISOPLANE_BUILD_DIRECTORY')
        if previous and inherited.get('ISOPLANE_BUILD_ROOT') != str(root):
            for name in ('CARGO_TARGET_DIR', 'CARGO_BUILD_BUILD_DIR'):
                if environment.get(name) == str(Path(previous) / 'target'):
                    environment.pop(name)
        environment.setdefault('CARGO_TARGET_DIR', str(directory / 'target'))
        environment.setdefault('CARGO_BUILD_BUILD_DIR', environment['CARGO_TARGET_DIR'])
        environment['ISOPLANE_BUILD_ROOT'] = str(root)
        environment['ISOPLANE_BUILD_DIRECTORY'] = str(directory)
        environment['ISOPLANE_RUNTIME_SOURCE'] = str(root / 'guest-runtime')
        environment['ISOPLANE_RUNTIME_WORK'] = str(directory / 'runtime-work')
        yield temporary_environment(directory / 'tmp', environment)


def temporary_environment(directory, inherited):
    """Keep subprocess scratch local without splitting NuGet's package-cache locks."""
    environment = dict(inherited)
    directory = Path(directory).resolve()
    directory.mkdir(parents=True, exist_ok=True)
    # NuGet's shared package cache requires a common lock namespace across worktrees.
    if 'NUGET_SCRATCH' not in environment:
        if os.name == 'nt' or platform.system() == 'Darwin':
            environment['NUGET_SCRATCH'] = str(Path(tempfile.gettempdir()) / 'NuGetScratch')
        else:
            import getpass
            environment['NUGET_SCRATCH'] = '/tmp/NuGetScratch' + getpass.getuser()
    for name in ('TMPDIR', 'TMP', 'TEMP'):
        environment[name] = str(directory)
    return environment



@contextmanager
def workspace(name, explicit=None):
    """Use this host's worktree storage unless a caller supplies a workspace."""
    if explicit is not None:
        path = Path(explicit).resolve()
        path.mkdir(parents=True, exist_ok=True)
        yield path
    else:
        directory_name = re.sub(r'[^a-zA-Z0-9._-]', '-', name)
        with product(f"workspace:{name}", root=build_root(), name=directory_name) as path:
            yield path


def digest(path, algorithm="sha256"):
    with Path(path).open("rb") as source:
        return hashlib.file_digest(source, algorithm).hexdigest()


def restore_files(entry, outputs):
    receipt = entry / 'files.json'
    files = [entry / str(index) for index in range(len(outputs))]
    try:
        hashes = json.loads(receipt.read_text())
        valid = isinstance(hashes, list) and len(hashes) == len(files) and all(
            digest(path) == expected for path, expected in zip(files, hashes))
    except (FileNotFoundError, json.JSONDecodeError):
        valid = False
    if not valid:
        return False
    for source, destination in zip(files, outputs):
        destination.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix='.product-', dir=destination.parent) as temporary:
            staged = Path(temporary) / 'file'
            shutil.copy2(source, staged)
            staged.replace(destination)
    return True


def artifact_directory(identity):
    return shared_root() / hashlib.sha256(identity.encode()).hexdigest()


def publish_files(identity, outputs):
    """Publish a complete artifact directory; consumers only read completed files."""
    entry = artifact_directory(identity)
    entry.parent.mkdir(parents=True, exist_ok=True)
    if entry.is_symlink():
        raise ValueError(f'shared artifact directory must not be a symlink: {entry}')
    hashes = [digest(path) for path in outputs]
    with tempfile.TemporaryDirectory(prefix='.publish-', dir=entry.parent) as temporary:
        staged = Path(temporary) / 'artifact'
        staged.mkdir()
        for index, source in enumerate(outputs):
            shutil.copy2(source, staged / str(index))
        (staged / 'files.json').write_text(json.dumps(hashes))
        (staged / 'owner.json').write_text(json.dumps({'identity': identity}))
        try:
            staged.rename(entry)
        except OSError as error:
            if error.errno not in (errno.EEXIST, errno.ENOTEMPTY) or not entry.is_dir():
                raise
            if json.loads((entry / 'files.json').read_text()) != hashes or any(
                    digest(entry / str(index)) != expected for index, expected in enumerate(hashes)):
                raise ValueError(f'Shared artifacts differ at {entry}; clean them before rebuilding') from error


def cached_files(identity, outputs, produce, linux_only=False):
    """Consume published files, or publish a completed build from its owning host."""
    outputs = [Path(path) for path in outputs]
    identity = 'files:' + identity
    entry = artifact_directory(identity)
    if restore_files(entry, outputs):
        return
    if produce is None or (linux_only and platform.system() != 'Linux'):
        raise RuntimeError('Shared build outputs are missing or stale. Build them on Linux in this worktree first.')
    if entry.exists():
        raise ValueError(f'Shared artifacts are invalid at {entry}; clean them before rebuilding')
    produce()
    publish_files(identity, outputs)


def clean_shared(dry_run=False):
    """Explicitly remove published artifacts after their consumers have stopped."""
    return clean(shared_root(), dry_run=dry_run)


def download(url, destination, expected, algorithm="sha256"):
    """Verify and publish downloads, with each consumer staging its own copy."""
    destination = Path(destination)
    if destination.is_file() and digest(destination, algorithm) == expected:
        return
    def fetch():
        destination.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix='.download-', dir=destination.parent) as temporary:
            pending = Path(temporary) / 'archive'
            with urllib.request.urlopen(url, timeout=60) as response, pending.open('wb') as output:
                shutil.copyfileobj(response, output)
            if digest(pending, algorithm) != expected:
                raise ValueError(f'download hash mismatch: {url}')
            pending.replace(destination)
    cached_files(f'download:{algorithm}:{expected}', [destination], fetch)


if __name__ == "__main__":
    import argparse
    import subprocess
    import sys
    parser = argparse.ArgumentParser(description=__doc__)
    if sys.argv[1:2] == ["runtime"]:
        root = Path(sys.argv[2]).resolve()
        with development(root, os.environ) as environment:
            raise SystemExit(subprocess.call(sys.argv[3:], env=environment))
    elif sys.argv[1:2] == ["files"]:
        parser.add_argument("identity")
        parser.add_argument("--linux-only", action="store_true")
        parser.add_argument("--output", type=Path, action="append", required=True)
        if "--" not in sys.argv:
            parser.error("files requires -- followed by the build command")
        separator = sys.argv.index("--")
        arguments = parser.parse_args(sys.argv[2:separator])
        command = sys.argv[separator + 1:]
        if not command:
            parser.error("files requires a build command")
        cached_files(arguments.identity, arguments.output,
                     lambda: subprocess.run(command, check=True),
                     linux_only=arguments.linux_only)
    else:
        parser.add_argument("url")
        parser.add_argument("destination", type=Path)
        parser.add_argument("sha256")
        arguments = parser.parse_args()
        download(arguments.url, arguments.destination, arguments.sha256)
