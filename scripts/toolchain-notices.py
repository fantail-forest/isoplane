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
import xml.etree.ElementTree as ET


PREFIXES = ('license', 'licence', 'copying', 'notice', 'copyright', 'third-party')


def fetch(url):
    with urllib.request.urlopen(url, timeout=60) as response:
        return response.read()


def license_files(directory):
    return {path.name: path.read_bytes() for path in directory.iterdir()
            if path.is_file() and path.name.lower().startswith(PREFIXES)}


def checkout_root(directory):
    """The git checkout a crate came from; its license texts cover every crate of the workspace inside it."""
    for candidate in [directory, *directory.parents]:
        if (candidate / '.cargo-ok').exists() or (candidate / '.git').exists():
            return candidate
    raise ValueError(f'no git checkout root above {directory}')


def source_licenses(package, cache, repositories):
    root = Path(package['manifest_path']).parent
    files = license_files(root)
    if package.get('license_file'):
        path = Path(package['license_file'])
        if not path.is_absolute():
            path = root / path
        files[path.name] = path.read_bytes()
    if not files and (package.get('source') or '').startswith('git+'):
        files = license_files(checkout_root(root))
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


def managed_notices(source, output):
    packages = {}
    for assets in sorted(source.rglob('project.assets.json')):
        document = json.loads(assets.read_text())
        for identity, library in document['libraries'].items():
            if library['type'] != 'package':
                continue
            for folder in document['packageFolders']:
                directory = Path(folder) / library['path']
                if directory.is_dir():
                    packages[identity] = directory
                    break
            else:
                raise ValueError(f'managed dependency is missing: {identity}')
    records = []
    license_cache = {}
    for identity, directory in sorted(packages.items()):
        name, version = identity.split('/')
        destination = output / 'managed' / name / version
        destination.mkdir(parents=True)
        nuspec = directory / (name.lower() + '.nuspec')
        shutil.copyfile(nuspec, destination / nuspec.name)
        metadata = ET.parse(nuspec).getroot().find('{*}metadata')
        license = metadata.find('{*}license')
        if license is not None and license.get('type') == 'file':
            path = directory / license.text
            shutil.copyfile(path, destination / path.name)
        else:
            url = ('https://licenses.nuget.org/' + urllib.parse.quote(license.text, safe='')
                   if license is not None else metadata.findtext('{*}licenseUrl'))
            if not url:
                raise ValueError(f'managed dependency has no license: {identity}')
            if url not in license_cache:
                license_cache[url] = fetch(url)
            (destination / 'LICENSE.html').write_bytes(license_cache[url])
        records.append({'id': name, 'version': version,
                        'license': license.text if license is not None else metadata.findtext('{*}licenseUrl')})
    (output / 'managed-dependencies.json').write_text(json.dumps(records, indent=2) + '\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--wasm-tools-source', type=Path, required=True)
    parser.add_argument('--runtime-work', type=Path, required=True)
    parser.add_argument('--wasi-sdk', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target', help='Rust target whose native dependencies are shipped (defaults to the build host)')
    parser.add_argument('--managed-notices', type=Path, required=True, help='Linux-built managed package attribution')
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    target = args.target or next(line.removeprefix('host: ') for line in subprocess.check_output(['rustc', '-vV'], text=True).splitlines() if line.startswith('host: '))
    packages = {}
    for manifest in (root / 'Cargo.toml', args.wasm_tools_source / 'Cargo.toml'):
        metadata = json.loads(subprocess.check_output(['cargo', 'metadata', '--locked', '--format-version', '1',
            '--filter-platform', target, '--manifest-path', str(manifest)]))
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
        for source in (args.runtime_work / ('dotnet-runtime' if component == 'runtime' else component)).iterdir():
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
    trust = args.output / 'trust'
    trust.mkdir(exist_ok=True)
    for name in ('NOTICE', 'ca-bundle.pem', 'ca-bundle.manifest.json'):
        shutil.copyfile(root / 'trust' / name, trust / name)
    shutil.copytree(args.managed_notices, args.output, dirs_exist_ok=True)
    print(args.output)


if __name__ == '__main__':
    main()
