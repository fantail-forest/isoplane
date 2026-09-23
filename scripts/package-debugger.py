#!/usr/bin/env python3
"""Assemble a relocatable LLDB and Python payload for the Isoplane toolchain."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tempfile

VERSION = '23.1.2'


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def runtime_identifier():
    system = {'Linux': 'linux', 'Windows': 'win', 'Darwin': 'osx'}[platform.system()]
    architecture = {'x86_64': 'x64', 'amd64': 'x64', 'aarch64': 'arm64', 'arm64': 'arm64'}[platform.machine().lower()]
    return system + '-' + architecture


def verify(root):
    receipt = json.loads((root / 'debugger.json').read_text())
    if receipt['version'] != VERSION:
        raise ValueError(f'expected LLDB {VERSION}')
    if receipt['runtimeIdentifier'] != runtime_identifier():
        raise ValueError('debugger payload targets a different host platform')
    extension = '.exe' if platform.system() == 'Windows' else ''
    required = {'bin/lldb' + extension, 'bin/lldb-dap' + extension, 'bin/lldb-server' + extension,
                'bindings/lldb/__init__.py', 'python/python.exe' if extension else 'python/bin/python3'}
    if platform.system() == 'Darwin':
        required.add('bin/debugserver')
    if not required.issubset(receipt['files']):
        raise ValueError('debugger payload is incomplete')
    for name, expected in receipt['files'].items():
        relative = Path(name)
        if relative.is_absolute() or '\\' in name or any(part in ('', '.', '..') for part in name.split('/')):
            raise ValueError(f'unsafe debugger payload path: {name}')
        if digest(root / relative) != expected:
            raise ValueError(f'debugger payload differs from its receipt: {name}')
    actual = {file.relative_to(root).as_posix() for file in root.rglob('*') if file.is_file() and file != root / 'debugger.json'}
    if actual != set(receipt['files']):
        raise ValueError('debugger file inventory differs from its receipt')
    return receipt


def assemble(llvm, python, bindings, output):
    if output.exists():
        raise ValueError(f'output already exists: {output}')
    executable = '.exe' if platform.system() == 'Windows' else ''
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.debugger-', dir=output.parent) as temporary:
        stage = Path(temporary) / 'payload'
        (stage / 'bin').mkdir(parents=True)
        (stage / 'lib').mkdir()
        for name in ('lldb', 'lldb-dap', 'lldb-server'):
            shutil.copy2(llvm / 'bin' / (name + executable), stage / 'bin' / (name + executable))
        if platform.system() == 'Darwin':
            shutil.copy2(llvm / 'bin/debugserver', stage / 'bin/debugserver')
        for file in (llvm / 'bin').iterdir():
            if file.suffix == '.dll' or file.name == 'lldb-argdumper' + executable:
                shutil.copy2(file, stage / 'bin' / file.name)
        for file in (llvm / 'lib').iterdir():
            if file.is_file() and ('.so' in file.suffixes or file.suffix == '.dylib'):
                shutil.copy2(file, stage / 'lib' / file.name)
        shutil.copytree(llvm / 'lib/clang', stage / 'lib/clang', symlinks=False)
        shutil.copytree(python, stage / 'python', symlinks=False)
        shutil.copytree(bindings / 'lldb', stage / 'bindings/lldb', symlinks=False)
        environment = dict(os.environ, PYTHONHOME=str(stage / 'python'), PYTHONPATH=str(stage / 'bindings'), PYTHONNOUSERSITE='1', PYTHONDONTWRITEBYTECODE='1')
        environment['PATH'] = os.pathsep.join((str(stage / 'bin'), str(stage / 'python'), environment.get('PATH', os.defpath)))
        if platform.system() == 'Linux':
            environment['LD_LIBRARY_PATH'] = os.pathsep.join((str(stage / 'lib'), str(stage / 'python/lib')))
        version = subprocess.check_output([str(stage / 'bin' / ('lldb' + executable)), '--version'], env=environment, text=True)
        if version.split()[:3] != ['lldb', 'version', VERSION]:
            raise ValueError(f'expected LLDB {VERSION}; got {version.strip()}')
        probe = subprocess.check_output([str(stage / 'bin' / ('lldb' + executable)), '--batch', '-o',
                                         'script import lldb, json, struct; print(lldb.SBTarget.__name__)'], env=environment, text=True)
        if 'SBTarget' not in probe.splitlines():
            raise ValueError('LLDB Python bindings could not load from the staged payload')
        receipt = {'version': VERSION, 'runtimeIdentifier': runtime_identifier(), 'files': {file.relative_to(stage).as_posix(): digest(file)
                   for file in sorted(stage.rglob('*')) if file.is_file()}}
        (stage / 'debugger.json').write_text(json.dumps(receipt, indent=2) + '\n')
        verify(stage)
        stage.rename(output)
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--llvm', type=Path, required=True, help='LLVM installation or build with Python-enabled LLDB')
    parser.add_argument('--python', type=Path, required=True, help='relocatable Python matching the LLDB build')
    parser.add_argument('--bindings', type=Path, required=True, help='LLDB site-packages directory')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    assemble(args.llvm.resolve(), args.python.resolve(), args.bindings.resolve(), args.output.resolve())


if __name__ == '__main__':
    main()
