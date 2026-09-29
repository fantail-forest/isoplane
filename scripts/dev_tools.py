"""Versioned development tools shared across worktrees on this host."""

import importlib.util
import json
import os
import platform
import re
import subprocess
from pathlib import Path

import build_storage as storage


def identity(root):
    sdk = json.loads((root / 'global.json').read_text())['sdk']['version']
    wasm = re.search(r'cargo install --locked wasm-tools@([^\s]+)', (root / 'Dockerfile').read_text()).group(1)
    rid = {'Linux': 'linux', 'Darwin': 'osx', 'Windows': 'win'}[platform.system()]
    rid += '-' + {'x86_64': 'x64', 'amd64': 'x64', 'aarch64': 'arm64', 'arm64': 'arm64'}[platform.machine().lower()]
    return f'development-tools:{sdk}:{wasm}:{rid}', wasm, rid


def preview_builder(root):
    """Load the shared SDK installation and inventory implementation."""
    spec = importlib.util.spec_from_file_location('preview', root / 'scripts/build-preview.py')
    preview = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(preview)
    return preview


def installation_ready(directory, name, wasm, sdk, root):
    """Check the completion identity and the installed tools before sharing them."""
    if directory is None:
        return False
    extension = '.exe' if os.name == 'nt' else ''
    try:
        if (directory / 'complete').read_text() != name:
            return False
        receipt = json.loads((directory / 'dotnet.json').read_text())
        if receipt['version'] != sdk or receipt['runtimeIdentifier'] != identity(root)[2]:
            return False
        preview_builder(root).verify_inventory(directory / 'dotnet', receipt['files'])
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
        (directory / 'complete').unlink(missing_ok=True)
        (directory / 'dotnet.json').unlink(missing_ok=True)
        with storage.workspace('tool-setup') as work:
            logs = work / 'logs'
            logs.mkdir(exist_ok=True)
            environment = storage.temporary_environment(work / 'tmp', os.environ)
            environment['CARGO_TARGET_DIR'] = str(work / 'target')
            environment['CARGO_BUILD_BUILD_DIR'] = str(work / 'target')
            preview.run(['cargo', 'install', '--locked', '--force', f'wasm-tools@{wasm}', '--root', directory],
                        root, logs / 'wasm-tools.log', environment)
            preview.prepare_sdk(root, directory, logs, rid)
        (directory / 'complete').write_text(name)
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
