"""Worktree-owned build output and explicit cleanup."""

from contextlib import contextmanager
import hashlib
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


def clean(root, dry_run=False):
    """Explicitly remove a disposable output directory."""
    root = Path(root)
    if not root.is_dir() or root.is_symlink():
        return []
    removed = [(root, size_bytes(root))]
    if not dry_run:
        shutil.rmtree(root)
    return removed


@contextmanager
def retained_run(root, remove_on_success=False):
    """Keep diagnostics until explicit cleanup; optionally remove successful scratch."""
    root = Path(root)
    root.mkdir(parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix="run-", dir=root))
    yield directory
    if remove_on_success:
        shutil.rmtree(directory)


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
    if directory.is_symlink():
        raise ValueError(f"product storage must not be a symlink: {directory}")
    if read_only:
        yield directory if directory.is_dir() else None
        return
    directory.mkdir(parents=True, exist_ok=True)
    yield directory


@contextmanager
def development(root, inherited):
    """Keep a host's mutable build and temporary files inside its worktree."""
    root = Path(root).resolve()
    with (product('development', root=build_root(root), name='development') as directory,
          product('managed', root=root / '.build/clr-anycpu', name='managed')):
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


def download(url, destination, expected, algorithm="sha256"):
    """Download an archive into its build directory and verify its checksum."""
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
    fetch()


if __name__ == "__main__":
    import argparse
    import subprocess
    import sys
    parser = argparse.ArgumentParser(description=__doc__)
    if sys.argv[1:2] == ["runtime"]:
        root = Path(sys.argv[2]).resolve()
        with development(root, os.environ) as environment:
            raise SystemExit(subprocess.call(sys.argv[3:], env=environment))

    else:
        parser.add_argument("url")
        parser.add_argument("destination", type=Path)
        parser.add_argument("sha256")
        arguments = parser.parse_args()
        download(arguments.url, arguments.destination, arguments.sha256)
