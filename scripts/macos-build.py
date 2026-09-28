#!/usr/bin/env python3
"""Acquire native macOS tools and reuse a verified platform-independent runtime build."""
import json
from pathlib import Path
import shutil
import tarfile
import tempfile


def prepare(preview, source, cache, logs, args):
    runtime = source / 'guest-runtime'
    output = runtime / 'out'
    output.mkdir(exist_ok=True)
    artifact = preview.load(source / 'scripts/runtime-artifact.py')
    with tempfile.TemporaryDirectory(prefix='runtime-', dir=cache) as temporary:
        imported = Path(temporary) / 'imported'
        artifact.import_artifact(args.runtime_artifact, args.runtime_sha256, imported)
        for name in ('feed', 'packs'):
            if (output / name).exists():
                shutil.rmtree(output / name)
            shutil.move(imported / name, output / name)
    work = runtime / 'work'
    work.mkdir(exist_ok=True)
    prepare_wasi(preview, source, cache, work)
    prepare_notices(preview, runtime, cache, work)
    return output


def prepare_wasi(preview, source, cache, work):
    specification = json.loads((source / 'scripts/macos-build-inputs.json').read_text())[preview.RUNTIME_IDENTIFIER]
    archive = cache / specification['url'].rsplit('/', 1)[-1]
    preview.download(specification['url'], archive, specification['sha256'])
    wasi = work / 'wasi-sdk'
    marker = wasi / '.isoplane-archive-sha256'
    if not marker.exists() or marker.read_text().strip() != specification['sha256']:
        with tempfile.TemporaryDirectory(prefix='wasi-sdk-', dir=cache) as temporary:
            with tarfile.open(archive) as bundle:
                bundle.extractall(temporary, filter='data')
            directories = list(Path(temporary).iterdir())
            if len(directories) != 1 or not (directories[0] / 'bin/clang').is_file():
                raise ValueError('WASI SDK archive does not contain one SDK directory')
            if wasi.exists():
                shutil.rmtree(wasi)
            shutil.move(directories[0], wasi)
        marker.write_text(specification['sha256'] + '\n')


def prepare_notices(preview, runtime, cache, work):
    values = dict(line.split('=', 1) for line in (runtime / 'manifest.env').read_text().splitlines()
                  if line and not line.startswith('#') and '=' in line)
    for component, prefix in [('runtime', 'RUNTIME_TARBALL'), ('openssl', 'OPENSSL_TARBALL'), ('icu', 'ICU_SOURCE')]:
        archive = cache / (component + '-notices.tar.gz')
        preview.download(values[prefix + '_URL'], archive, values[prefix + '_SHA256'])
        destination = work / component
        destination.mkdir(exist_ok=True)
        with tarfile.open(archive) as bundle:
            for member in bundle.getmembers():
                path = Path(member.name)
                if member.isfile() and len(path.parts) == 2 and path.name.lower().startswith(
                        ('license', 'licence', 'copying', 'notice', 'copyright', 'third-party')):
                    with bundle.extractfile(member) as original, (destination / path.name).open('wb') as target:
                        shutil.copyfileobj(original, target)
