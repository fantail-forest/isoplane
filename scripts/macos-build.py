#!/usr/bin/env python3
"""Acquire native macOS tools and reuse a verified platform-independent runtime build."""
import json
from pathlib import Path
import shutil
import tarfile
import tempfile


def prepare_wasi(preview, source, cache, work):
    specification = json.loads((source / 'scripts/macos-build-inputs.json').read_text())[preview.RUNTIME_IDENTIFIER]
    archive = cache / specification['url'].rsplit('/', 1)[-1]
    preview.download(specification['url'], archive, specification['sha256'])
    work.mkdir(parents=True, exist_ok=True)
    wasi = work / 'wasi-sdk'
    version = next(line.split('=', 1)[1] for line in (source / 'guest-runtime/manifest.env').read_text().splitlines() if line.startswith('WASI_SDK_VERSION='))
    if not (wasi / 'VERSION').is_file() or (wasi / 'VERSION').read_text().splitlines()[0] != version:
        with tempfile.TemporaryDirectory(prefix='wasi-sdk-', dir=cache) as temporary:
            with tarfile.open(archive) as bundle:
                bundle.extractall(temporary, filter='data')
            directories = list(Path(temporary).iterdir())
            if len(directories) != 1 or not (directories[0] / 'bin/clang').is_file():
                raise ValueError('WASI SDK archive does not contain one SDK directory')
            if wasi.exists():
                shutil.rmtree(wasi)
            shutil.move(directories[0], wasi)
