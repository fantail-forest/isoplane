"""Versioned development tools shared across worktrees on this host."""

import importlib.util
import json
import os
import platform
import re
import subprocess
import tarfile
import urllib.request
import zipfile
from pathlib import Path

import build_storage as storage

TASK_VERSION = '3.53.1'


def install_task(root):
    """Install the pinned Task executable in the user's tool directory."""
    executable = Path.home() / '.local/bin' / ('task.exe' if os.name == 'nt' else 'task')
    if executable.is_file():
        result = subprocess.run([str(executable), '--version'], text=True, capture_output=True)
        if result.returncode == 0 and result.stdout.strip() == TASK_VERSION:
            return
    system = {'Linux': 'linux', 'Darwin': 'darwin', 'Windows': 'windows'}[platform.system()]
    arch = {'x86_64': 'amd64', 'amd64': 'amd64', 'aarch64': 'arm64', 'arm64': 'arm64'}[platform.machine().lower()]
    suffix = 'zip' if os.name == 'nt' else 'tar.gz'
    name = f'task_{system}_{arch}.{suffix}'
    url = f'https://github.com/go-task/task/releases/download/v{TASK_VERSION}'
    with urllib.request.urlopen(url + '/task_checksums.txt', timeout=60) as response:
        checksums = dict((line.split()[1], line.split()[0]) for line in response.read().decode().splitlines())
    archive = storage.build_root(root) / 'tool-setup' / name
    storage.download(url + '/' + name, archive, checksums[name])
    executable.parent.mkdir(parents=True, exist_ok=True)
    if os.name == 'nt':
        with zipfile.ZipFile(archive) as bundle:
            executable.write_bytes(bundle.read('task.exe'))
    else:
        with tarfile.open(archive) as bundle:
            executable.write_bytes(bundle.extractfile('task').read())
        executable.chmod(0o755)


def identity(root):
    sdk = json.loads((root / 'global.json').read_text())['sdk']['version']
    wasm = re.search(r'cargo install --locked wasm-tools@([^\s]+)', (root / 'Dockerfile').read_text()).group(1)
    rid = {'Linux': 'linux', 'Darwin': 'osx', 'Windows': 'win'}[platform.system()]
    rid += '-' + {'x86_64': 'x64', 'amd64': 'x64', 'aarch64': 'arm64', 'arm64': 'arm64'}[platform.machine().lower()]
    return f'development-tools:{sdk}:{wasm}:{rid}', wasm, rid


def preview_builder(root):
    """Load the SDK installer."""
    spec = importlib.util.spec_from_file_location('preview', root / 'scripts/build-preview.py')
    preview = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(preview)
    return preview


def installation_ready(directory, name, wasm, sdk, root):
    """Ask installed tools for their versions."""
    if directory is None:
        return False
    extension = '.exe' if os.name == 'nt' else ''
    try:
        for executable, expected in ((directory / 'bin' / ('wasm-tools' + extension), 'wasm-tools ' + wasm),
                                     (directory / 'dotnet' / ('dotnet' + extension), sdk)):
            actual = subprocess.check_output([str(executable), '--version'], cwd=root, text=True,
                                             stderr=subprocess.PIPE, timeout=30).strip()
            if actual != expected:
                return False
        return True
    except (OSError, ValueError, KeyError, TypeError, subprocess.CalledProcessError, subprocess.TimeoutExpired):
        return False


def setup(root):
    subprocess.run(['git', 'submodule', 'update', '--init'], cwd=root, check=True)
    install_task(root)
    name, wasm, rid = identity(root)
    sdk = json.loads((root / 'global.json').read_text())['sdk']['version']
    with storage.product(name, root=storage.tools_root(), read_only=True) as directory:
        if installation_ready(directory, name, wasm, sdk, root):
            ensure_rust_targets(root)
            print('Development tools are already installed and verified.')
            return
    if directory is not None and os.environ.get('ISOPLANE_DEV_TOOLS') == str(directory):
        raise SystemExit('Development tools need repair, but this development session is using them. '
                         'Exit the development shell and run scripts/setup directly.')
    ensure_rust_targets(root)
    preview = preview_builder(root)
    with storage.product(name, root=storage.tools_root()) as directory:
        if installation_ready(directory, name, wasm, sdk, root):
            print('Development tools are already installed and verified.')
            return
        with storage.workspace('tool-setup') as work:
            logs = work / 'logs'
            logs.mkdir(exist_ok=True)
            environment = storage.temporary_environment(work / 'tmp', os.environ)
            environment['CARGO_TARGET_DIR'] = str(work / 'target')
            environment['CARGO_BUILD_BUILD_DIR'] = str(work / 'target')
            preview.run(['cargo', 'install', '--locked', '--force', f'wasm-tools@{wasm}', '--root', directory],
                        root, logs / 'wasm-tools.log', environment)
            preview.prepare_sdk(root, directory, logs, rid)
    print('Development tools installed in the shared user cache.')


def ensure_rust_targets(root):
    """Provision guest targets for the currently selected Rust toolchain."""
    with storage.records() as directory:
        log = directory / 'rust-targets.log'
        with log.open('w') as output:
            result = subprocess.run(['rustup', 'target', 'add', 'wasm32-unknown-unknown', 'wasm32-wasip1'],
                                    cwd=root, stdout=output, stderr=subprocess.STDOUT)
        if result.returncode:
            raise RuntimeError(f'Installing Rust guest targets failed; see {log}')


if __name__ == '__main__':
    setup(Path(__file__).resolve().parents[1])
