#!/usr/bin/env python3
"""Validate shared runtime inputs before packing or transferring them to another host."""
import hashlib
import importlib.util
import json
from pathlib import Path
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]


def module_names():
    """Read the module inventory from its receipt producer."""
    spec = importlib.util.spec_from_file_location('runtime_receipt', ROOT / 'guest-sdks/dotnet/MonoRuntime/receipt.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.MODULES


def files(directory):
    """Return the complete portable runtime input set."""
    return [directory / name for name in ('runtime-build.json', *module_names(), *(name + '.runtime' for name in module_names()))]


def verify(directory, version, revision, feed=None):
    """Reject stale or changed modules, including mismatched linked Mono packs."""
    receipt = json.loads((directory / 'runtime-build.json').read_text())
    if receipt['sdkVersion'] != version or receipt['sourceRevision'] != revision:
        raise ValueError('shared runtime SDK version or source revision mismatch')
    if set(receipt['modules']) != set(module_names()):
        raise ValueError('shared runtime module inventory mismatch')
    if feed is not None:
        props = ET.parse(feed / 'Isoplane.GuestRuntime.props').getroot()
        expected = props.findtext('./PropertyGroup/IsoplaneGuestRuntimePackVersion') + '@sha512:' + props.findtext('./PropertyGroup/IsoplaneGuestRuntimePackSha512')
        if receipt['monoPack'] != expected:
            raise ValueError('shared runtime Mono pack differs from the selected feed')
    for name in module_names():
        path = directory / name
        if path.is_symlink() or path.stat().st_size == 0:
            raise ValueError('shared runtime must be a nonempty regular module')
        with path.open('rb') as stream:
            if hashlib.file_digest(stream, 'sha256').hexdigest() != receipt['modules'][name]:
                raise ValueError('shared runtime module SHA256 mismatch')
        if (directory / (name + '.runtime')).read_text().strip() != receipt['monoPack']:
            raise ValueError('shared runtime Mono pack sidecar mismatch')
    return receipt
