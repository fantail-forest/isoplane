#!/usr/bin/env python3
"""Collect dependency license texts and attribution for a toolchain bundle."""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import urllib.error
import urllib.parse
import urllib.request


PREFIXES = ('license', 'licence', 'copying', 'notice', 'copyright', 'third-party')


def fetch(url):
    with urllib.request.urlopen(url, timeout=60) as response:
        return response.read()


def source_licenses(package, cache, repositories):
    root = Path(package['manifest_path']).parent
    files = {path.name: path.read_bytes() for path in root.iterdir()
             if path.is_file() and path.name.lower().startswith(PREFIXES)}
    if package.get('license_file'):
        path = Path(package['license_file'])
        if not path.is_absolute():
            path = root / path
        files[path.name] = path.read_bytes()
    if files:
        return files
    vcs = root / '.cargo_vcs_info.json'
    revision = json.loads(vcs.read_text())['git']['sha1'] if vcs.exists() else None
    repository = urllib.parse.urlparse(package.get('repository') or repositories.get(revision, ''))
    if repository.hostname != 'github.com' or not vcs.exists():
        headers = []
        for source in sorted(root.rglob('*.rs')):
            lines = []
            for line in source.read_text().splitlines():
                if line and not line.startswith('//'):
                    break
                lines.append(line)
            header = '\n'.join(lines)
            if 'copyright' in header.lower() and 'license' in header.lower():
                headers.append(f'{source.relative_to(root)}\n{header}')
        if headers:
            return {'COPYRIGHT-HEADERS.txt': '\n\n'.join(headers).encode()}
        raise ValueError(f"No packaged license texts for {package['name']} {package['version']}")
    revision = json.loads(vcs.read_text())['git']['sha1']
    repo = '/'.join(repository.path.strip('/').removesuffix('.git').split('/')[:2])
    key = (repo, revision)
    if key not in cache:
        url = f'https://api.github.com/repos/{repo}/contents?ref={revision}'
        entries = json.loads(fetch(url))
        texts = {}
        for item in entries:
            if not item['name'].lower().startswith(PREFIXES):
                continue
            if item['type'] == 'file':
                texts[item['name']] = fetch(item['download_url'])
            elif item['type'] == 'dir':
                for child in json.loads(fetch(item['url'])):
                    if child['type'] == 'file':
                        texts[item['name'] + '-' + child['name']] = fetch(child['download_url'])
        cache[key] = texts
    if not cache[key]:
        raise ValueError(f'No license texts at {repo}@{revision}')
    return cache[key]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--wasm-tools-source', type=Path, required=True)
    parser.add_argument('--runtime-work', type=Path, required=True)
    parser.add_argument('--wasi-sdk', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    packages = {}
    for manifest in (root / 'Cargo.toml', args.wasm_tools_source / 'Cargo.toml'):
        metadata = json.loads(subprocess.check_output(['cargo', 'metadata', '--locked', '--format-version', '1',
            '--filter-platform', 'x86_64-unknown-linux-gnu', '--manifest-path', str(manifest)]))
        for package in metadata['packages']:
            if package['source'] is not None or package['name'] == 'wasm-tools':
                packages[(package['name'], package['version'])] = package
    args.output.mkdir(parents=True, exist_ok=True)
    cache = {}
    repositories = {}
    for package in packages.values():
        vcs = Path(package['manifest_path']).parent / '.cargo_vcs_info.json'
        if package.get('repository') and vcs.exists():
            repositories[json.loads(vcs.read_text())['git']['sha1']] = package['repository']
    attribution = []
    for (name, version), package in sorted(packages.items()):
        directory = args.output / 'rust' / f'{name}-{version}'
        texts = {p.name: p.read_bytes() for p in directory.iterdir()} if directory.exists() else source_licenses(package, cache, repositories)
        directory.mkdir(parents=True, exist_ok=True)
        for filename, text in texts.items():
            (directory / filename).write_bytes(text)
        attribution.append({key: package[key] for key in ('name', 'version', 'license', 'authors', 'repository')})
    (args.output / 'rust-dependencies.json').write_text(json.dumps(attribution, indent=2) + '\n')
    for component in ('openssl', 'icu', 'runtime'):
        for source in (args.runtime_work / component).iterdir():
            if source.is_file() and source.name.lower().startswith(PREFIXES):
                destination = args.output / component / source.name
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(source, destination)
    version_lines = (args.wasi_sdk / 'VERSION').read_text().splitlines()
    revisions = dict(line.split(': ', 1) for line in version_lines[1:])
    sources = [('WebAssembly/wasi-sdk', 'wasi-sdk-' + version_lines[0].split('.')[0], 'LICENSE'),
               ('llvm/llvm-project', revisions['llvm'], 'llvm/LICENSE.TXT'),
               ('llvm/llvm-project', revisions['llvm'], 'compiler-rt/LICENSE.TXT')]
    for filename in ('LICENSE', 'LICENSE-APACHE', 'LICENSE-APACHE-LLVM', 'LICENSE-MIT'):
        sources.append(('WebAssembly/wasi-libc', revisions['wasi-libc'], filename))
    for repository, revision, filename in sources:
        destination = args.output / 'wasi-sdk' / repository.split('/')[-1] / filename
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(fetch(f'https://raw.githubusercontent.com/{repository}/{revision}/{filename}'))
    print(args.output)


if __name__ == '__main__':
    main()
