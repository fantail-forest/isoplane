#!/usr/bin/env python3
"""Validate shared runtime inputs before packing or transferring them to another host."""
import hashlib
import importlib.util
import json
from pathlib import Path
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]


def producer():
    """The SDK runtime manifest producer."""
    spec = importlib.util.spec_from_file_location('runtime_manifest', ROOT / 'guest-sdks/dotnet/MonoRuntime/manifest.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def module_names():
    """Read the module names from the manifest producer."""
    return producer().MODULES


def verify(directory, version, revision, feed=None):
    """Reject stale or changed modules, including mismatched linked Mono packs."""
    receipt = json.loads((directory / producer().PROVENANCE).read_text())
    if receipt['sdkVersion'] != version or receipt['sourceRevision'] != revision:
        raise ValueError('shared runtime SDK version or source revision mismatch')
    if set(receipt['modules']) != set(module_names()):
        raise ValueError('shared runtime module inventory mismatch')
    if feed is not None:
        props = ET.parse(feed / 'Isoplane.GuestRuntime.props').getroot()
        expected = props.findtext('./PropertyGroup/IsoplaneGuestRuntimePackVersion') + '@sha512:' + props.findtext('./PropertyGroup/IsoplaneGuestRuntimePackSha512')
        if receipt['monoPack']['version'] + '@sha512:' + receipt['monoPack']['sha512'] != expected:
            raise ValueError('shared runtime Mono pack differs from the selected feed')
    for name in module_names():
        path = directory / name
        if path.is_symlink() or path.stat().st_size == 0:
            raise ValueError('shared runtime must be a nonempty regular module')
        with path.open('rb') as stream:
            if hashlib.file_digest(stream, 'sha256').hexdigest() != receipt['modules'][name]['sha256']:
                raise ValueError('shared runtime module SHA256 mismatch')
        if (directory / (name + '.runtime')).read_text().strip() != receipt['monoPack']['version'] + '@sha512:' + receipt['monoPack']['sha512']:
            raise ValueError('shared runtime Mono pack sidecar mismatch')
    return receipt
