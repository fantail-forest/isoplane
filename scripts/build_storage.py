"""Owned build output with process leases and automatic retention."""

from contextlib import contextmanager
import errno
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import threading
import urllib.request


LOG_BYTES = 1024 ** 3
LOG_AGE_SECONDS = 7 * 24 * 60 * 60
PRODUCT_BYTES = 20 * 1024 ** 3
_handles = set()
_handles_lock = threading.Lock()


@contextmanager
def inherited_lease(handle):
    with _handles_lock:
        _handles.add(handle.fileno())
    try:
        yield
    finally:
        with _handles_lock:
            _handles.remove(handle.fileno())


def subprocess_options():
    """Keep POSIX child builds leased even if their coordinating process exits."""
    with _handles_lock:
        return {"pass_fds": tuple(_handles)} if os.name != "nt" else {}


def lock(handle, blocking=True, shared=False):
    if os.name == "nt":
        return windows_lock(handle, blocking, shared)
    else:
        import fcntl
        try:
            fcntl.flock(handle, (fcntl.LOCK_SH if shared else fcntl.LOCK_EX) | (0 if blocking else fcntl.LOCK_NB))
        except BlockingIOError:
            return False
    return True


def windows_lock(handle, blocking, shared):
    """Use kernel shared locks; the C runtime's byte locks are exclusive only."""
    import ctypes
    from ctypes import wintypes
    import msvcrt

    class Overlapped(ctypes.Structure):
        _fields_ = [("Internal", ctypes.c_size_t), ("InternalHigh", ctypes.c_size_t),
                    ("Offset", wintypes.DWORD), ("OffsetHigh", wintypes.DWORD),
                    ("hEvent", wintypes.HANDLE)]

    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    acquire = kernel.LockFileEx
    acquire.argtypes = [wintypes.HANDLE, wintypes.DWORD, wintypes.DWORD,
                        wintypes.DWORD, wintypes.DWORD, ctypes.POINTER(Overlapped)]
    acquire.restype = wintypes.BOOL
    flags = (0 if shared else 2) | (0 if blocking else 1)
    overlapped = Overlapped()
    if acquire(msvcrt.get_osfhandle(handle.fileno()), flags, 0, 1, 0, ctypes.byref(overlapped)):
        return True
    error = ctypes.get_last_error()
    if not blocking and error == 33:  # ERROR_LOCK_VIOLATION
        return False
    raise ctypes.WinError(error)


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


@contextmanager
def catalog(root):
    root = Path(root)
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    if root.is_symlink() or (root / ".storage.lock").is_symlink():
        raise ValueError(f"build storage must not be a symlink: {root}")
    with (root / ".storage.lock").open("a+b") as handle:
        lock(handle)
        yield


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


def prune(root, max_bytes, max_age_seconds=None, now=None, protected=(), dry_run=False):
    """Remove oldest inactive owned entries; unknown directories are never adopted."""
    now = time.time() if now is None else now
    root = Path(root)
    removed = []
    with catalog(root):
        entries = []
        for directory in root.iterdir():
            marker = directory / ".lease"
            if directory.is_symlink() or not directory.is_dir() or not marker.is_file() or marker.is_symlink():
                continue
            entries.append((marker.stat().st_mtime, directory, size_bytes(directory)))
        total = sum(size for _, _, size in entries)
        for touched, directory, size in sorted(entries):
            expired = max_age_seconds is not None and now - touched > max_age_seconds
            if directory in protected or (not expired and total <= max_bytes):
                continue
            with (directory / ".lease").open("r+b") as handle:
                if not lock(handle, blocking=False):
                    continue
                reason = source_preservation_reason(directory)
                if reason:
                    print(f'Retained {directory}: {reason}', file=sys.stderr)
                    continue
                # Windows cannot unlink the open lease. The catalog excludes new owners.
            if not dry_run:
                shutil.rmtree(directory)
            removed.append((directory, size))
            total -= size
    return removed


@contextmanager
def retained_run(root, max_bytes=LOG_BYTES, max_age_seconds=LOG_AGE_SECONDS, now=None,
                 remove_on_success=False):
    """Lease a run until completion, then retain its diagnostics within the budget."""
    root = Path(root)
    prune(root, max_bytes, max_age_seconds, now)
    with catalog(root):
        directory = Path(tempfile.mkdtemp(prefix="run-", dir=root))
        handle = (directory / ".lease").open("a+b")
        lock(handle)
    succeeded = False
    try:
        with inherited_lease(handle):
            yield directory, handle.fileno()
        succeeded = True
    finally:
        with catalog(root):
            touched = time.time() if now is None else now
            os.utime(directory / ".lease", (touched, touched))
            handle.close()
            if succeeded and remove_on_success:
                with (directory / ".lease").open("r+b") as released:
                    removable = lock(released, blocking=False)
                if removable:
                    shutil.rmtree(directory)
        prune(root, max_bytes, max_age_seconds, now)


def shared_root():
    """Use the platform's standard user cache directory, shared across worktrees."""
    if os.name == "nt":
        base = Path(os.environ.get("LOCALAPPDATA", Path.home() / "AppData/Local"))
    elif platform.system() == "Darwin":
        base = Path.home() / "Library/Caches"
    else:
        base = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache"))
    return base / "isoplane/build-products"


def diagnostics_root():
    """One diagnostic budget shared by runs from all worktrees."""
    return shared_root().parent / "diagnostics"


@contextmanager
def records(explicit=None):
    """Remove successful scratch runs; retain failures under the diagnostic budget."""
    if explicit is not None:
        directory = Path(explicit).resolve()
        directory.mkdir(parents=True, exist_ok=True)
        yield directory
    else:
        with retained_run(diagnostics_root(), remove_on_success=True) as (directory, _):
            yield directory


@contextmanager
def product(identity, root=None, read_only=False, shared=False):
    """Lease a product, sharing readers or locking a producer, and prune on release."""
    root = shared_root() if root is None else Path(root)
    name = hashlib.sha256(identity.encode()).hexdigest()
    # Wait outside the catalog: an owner needs it to release its lease.
    while True:
        with catalog(root):
            directory = root / name
            if read_only and not (directory / ".lease").is_file():
                handle = None
                break
            if directory.is_symlink() or (directory / ".lease").is_symlink():
                raise ValueError(f"product storage must not be a symlink: {directory}")
            if directory.exists() and not (directory / ".lease").is_file():
                raise ValueError(f"product storage is not owned: {directory}")
            directory.mkdir(exist_ok=True, mode=0o700)
            handle = (directory / ".lease").open("a+b")
            if lock(handle, blocking=False, shared=read_only or shared):
                if not read_only and (not shared or not (directory / 'owner.json').exists()):
                    pending = directory / ".owner.pending"
                    pending.write_text(json.dumps({"identity": identity, "workspace": str(Path.cwd()),
                                                   "pid": os.getpid()}))
                    pending.replace(directory / "owner.json")
                break
        # Wait for the current owner, then recheck under the catalog in case it was evicted.
        lock(handle, shared=read_only or shared)
        handle.close()
    if handle is None:
        yield None
        return
    try:
        prune(root, PRODUCT_BYTES)
        with inherited_lease(handle):
            yield directory
    finally:
        with catalog(root):
            os.utime(directory / ".lease", None)
            handle.close()
        prune(root, PRODUCT_BYTES)


@contextmanager
def development(root, inherited):
    """Lease a worktree's build intermediates under the shared product budget."""
    root = Path(root).resolve()
    with product('development:' + str(root), shared=True) as directory:
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
        yield environment


@contextmanager
def workspace(name, explicit=None):
    """Use a bounded shared build workspace unless the caller selects its own location."""
    if explicit is not None:
        path = Path(explicit).resolve()
        path.mkdir(parents=True, exist_ok=True)
        yield path
    else:
        with product(f"workspace:{name}") as path:
            yield path


def digest(path, algorithm="sha256"):
    with Path(path).open("rb") as source:
        return hashlib.file_digest(source, algorithm).hexdigest()


def cached_files(identity, outputs, produce):
    """Reuse verified immutable products, retaining independent consumer files."""
    outputs = [Path(path) for path in outputs]
    with product("files:" + identity) as entry:
        receipt = entry / "files.json"
        files = [entry / str(index) for index in range(len(outputs))]
        try:
            hashes = json.loads(receipt.read_text())
            valid = isinstance(hashes, list) and len(hashes) == len(files) and all(
                digest(path) == expected for path, expected in zip(files, hashes))
        except (FileNotFoundError, json.JSONDecodeError):
            valid = False
        if not valid:
            produce()
            for source, destination in zip(outputs, files):
                shutil.copy2(source, destination)
            receipt.write_text(json.dumps([digest(path) for path in files]))
        else:
            for source, destination in zip(files, outputs):
                destination.parent.mkdir(parents=True, exist_ok=True)
                with tempfile.TemporaryDirectory(prefix=".product-", dir=destination.parent) as temporary:
                    staged = Path(temporary) / "file"
                    shutil.copy2(source, staged)
                    staged.replace(destination)


def link_or_copy(source, destination):
    """Share immutable bytes on one filesystem, copying across filesystem boundaries."""
    try:
        os.link(source, destination)
    except OSError as error:
        if error.errno not in (errno.EXDEV, errno.EPERM, errno.EACCES, errno.ENOTSUP):
            raise
        shutil.copy2(source, destination)


def download(url, destination, expected, algorithm="sha256"):
    """Verify downloads once per content identity and atomically stage a consumer copy."""
    destination = Path(destination)
    if destination.is_file() and digest(destination, algorithm) == expected:
        return
    with product(f"download:{algorithm}:{expected}") as entry:
        archive = entry / "archive"
        if not archive.is_file() or digest(archive, algorithm) != expected:
            pending = entry / "download.pending"
            try:
                with urllib.request.urlopen(url, timeout=60) as response, pending.open("wb") as output:
                    shutil.copyfileobj(response, output)
                if digest(pending, algorithm) != expected:
                    raise ValueError(f"download hash mismatch: {url}")
                pending.replace(archive)
            finally:
                pending.unlink(missing_ok=True)
        destination.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix=".download-", dir=destination.parent) as temporary:
            staged = Path(temporary) / "archive"
            link_or_copy(archive, staged)
            staged.replace(destination)


if __name__ == "__main__":
    import argparse
    import subprocess
    import sys
    parser = argparse.ArgumentParser(description=__doc__)
    if sys.argv[1:2] == ["runtime"]:
        root = Path(sys.argv[2]).resolve()
        with development(root, os.environ) as environment:
            raise SystemExit(subprocess.call(sys.argv[3:], env=environment, **subprocess_options()))
    elif sys.argv[1:2] == ["files"]:
        parser.add_argument("identity")
        parser.add_argument("--output", type=Path, action="append", required=True)
        if "--" not in sys.argv:
            parser.error("files requires -- followed by the build command")
        separator = sys.argv.index("--")
        arguments = parser.parse_args(sys.argv[2:separator])
        command = sys.argv[separator + 1:]
        if not command:
            parser.error("files requires a build command")
        cached_files(arguments.identity, arguments.output,
                     lambda: subprocess.run(command, check=True, **subprocess_options()))
    else:
        parser.add_argument("url")
        parser.add_argument("destination", type=Path)
        parser.add_argument("sha256")
        arguments = parser.parse_args()
        download(arguments.url, arguments.destination, arguments.sha256)
