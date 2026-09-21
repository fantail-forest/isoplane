#!/usr/bin/env python3
"""Package an assembled Isoplane toolchain for the .NET workload installer."""

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import xml.etree.ElementTree as ET
import zipfile


def metadata(package_id, version):
    package = ET.Element('package')
    values = ET.SubElement(package, 'metadata')
    for key, value in [('id', package_id), ('version', version), ('authors', 'Isoplane'),
                       ('description', 'Isoplane application build tools')]:
        ET.SubElement(values, key).text = value
    return ET.tostring(package, encoding='utf-8', xml_declaration=True)


def sdk_properties():
    project = ET.Element('Project')
    properties = ET.SubElement(project, 'PropertyGroup')
    ET.SubElement(properties, 'IsoplaneWorkloadInstalled').text = 'true'
    for name, relative in [('IsoplaneGuestRuntimeFeed', '../feed'), ('WASI_SDK_PATH', '../wasi-sdk'),
                           ('IsoplaneCliAssembly', '../cli/Isoplane.Cli.dll')]:
        ET.SubElement(properties, name, Condition=f"'$({name})' == ''").text = (
            f"$([MSBuild]::NormalizePath('$(MSBuildThisFileDirectory)', '{relative}'))")
    return ET.tostring(project, encoding='utf-8', xml_declaration=True)


def package(toolchain, templates, version, output):
    manifest = json.loads((toolchain / 'toolchain.json').read_text())
    rid = manifest['runtimeIdentifier']
    package_id = f'Isoplane.Toolchain.{rid}'
    stock_version = manifest['wasiWorkloadVersion']
    stock = {
        'Isoplane.MonoTargets': 'Microsoft.NET.Runtime.MonoTargets.Sdk',
        'Isoplane.Wasi.Cross': f'Microsoft.NETCore.App.Runtime.AOT.{rid}.Cross.wasi-wasm',
    }
    output.mkdir(parents=True, exist_ok=False)
    feed = output / 'packages'
    feed.mkdir()
    with zipfile.ZipFile(feed / f'{package_id}.{version}.nupkg', 'w', zipfile.ZIP_DEFLATED) as archive:
        archive.writestr(f'{package_id}.nuspec', metadata(package_id, version))
        generated = {'Sdk/Sdk.props': sdk_properties(), 'Sdk/Sdk.targets': b'<Project />\n'}
        for name, content in generated.items():
            archive.writestr(name, content)
        for entry in manifest['files']:
            relative = entry['path']
            if relative.startswith('dotnet/'):
                continue
            archive.write(toolchain / relative, relative)
        installed = dict(manifest, files=[entry for entry in manifest['files'] if not entry['path'].startswith('dotnet/')])
        installed['files'].extend({'path': name, 'sha256': hashlib.sha256(content).hexdigest(), 'executable': False}
                                  for name, content in generated.items())
        archive.writestr('toolchain.json', json.dumps(installed, indent=2) + '\n')
    packs = {
        'Isoplane.Toolchain': {'kind': 'sdk', 'version': version, 'alias-to': {rid: package_id}},
        'Isoplane.Templates': {'kind': 'template', 'version': version},
    }
    for name, source_id in stock.items():
        source = toolchain / 'dotnet/packs' / source_id / stock_version
        if not (source / 'Sdk/Sdk.props').is_file():
            raise ValueError(f'The assembled toolchain is missing {source_id} {stock_version}')
        pack_id = f'{name}.{rid}'
        with zipfile.ZipFile(feed / f'{pack_id}.{version}.nupkg', 'w', zipfile.ZIP_DEFLATED) as archive:
            archive.writestr(f'{pack_id}.nuspec', metadata(pack_id, version))
            for file in sorted(source.rglob('*')):
                if file.is_file() and file.suffix not in ('.nupkg', '.nuspec'):
                    archive.write(file, file.relative_to(source).as_posix())
        packs[name] = {'kind': 'sdk', 'version': version, 'alias-to': {rid: pack_id}}
    shutil.copyfile(templates, feed / f'Isoplane.Templates.{version}.nupkg')
    definition = {
        'version': version,
        'workloads': {'isoplane': {
            'description': 'Isoplane application development',
            'platforms': [rid],
            'packs': list(packs),
        }},
        'packs': packs,
    }
    (output / 'WorkloadManifest.json').write_text(json.dumps(definition, indent=2) + '\n')
    (output / 'WorkloadManifest.targets').write_text('<Project />\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--toolchain', type=Path, required=True)
    parser.add_argument('--templates', type=Path, required=True)
    parser.add_argument('--version', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    package(args.toolchain, args.templates, args.version, args.output)


if __name__ == '__main__':
    main()
