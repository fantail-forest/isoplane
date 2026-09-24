import importlib.util
import json
import os
import subprocess
import sys
from pathlib import Path
import tempfile
import unittest
import xml.etree.ElementTree as ET
import zipfile

spec = importlib.util.spec_from_file_location('package_workload', Path(__file__).parents[1] / 'package-workload.py')
workload = importlib.util.module_from_spec(spec)
spec.loader.exec_module(workload)


class WorkloadPackageTests(unittest.TestCase):
    def test_package_dates_do_not_depend_on_timezone_or_future_source_dates(self):
        with tempfile.TemporaryDirectory(prefix='isoplane-package-dates-') as temporary:
            root = Path(temporary)
            source = root / 'tool'
            source.write_bytes(b'executable payload')
            source.chmod(0o755)
            os.utime(source, (2208988800, 2208988800))
            archives = []
            for index, timezone in enumerate(('UTC', 'Pacific/Auckland', 'America/Los_Angeles')):
                destination = root / f'{index}.zip'
                subprocess.run([sys.executable, '-c',
                                "import pathlib, runpy, sys, zipfile; "
                                "m = runpy.run_path(sys.argv[1]); "
                                "z = zipfile.ZipFile(sys.argv[3], 'w'); "
                                "m['write_package_file'](z, pathlib.Path(sys.argv[2]), 'tool'); "
                                "z.writestr(m['package_entry']('Sdk/Sdk.props'), b'<Project />'); z.close()",
                                str(Path(workload.__file__).resolve()), str(source), str(destination)],
                               env=dict(os.environ, TZ=timezone), check=True, timeout=30)
                with zipfile.ZipFile(destination) as archive:
                    self.assertEqual(archive.read('tool'), source.read_bytes(), 'timestamp normalization must preserve tool contents')
                    self.assertTrue(archive.getinfo('tool').external_attr >> 16 & 0o111, 'timestamp normalization must preserve executable permissions')
                    for entry in archive.infolist():
                        self.assertEqual(entry.date_time, (1980, 1, 1, 0, 0, 0), 'pack inputs must remain older than build outputs regardless of extraction timezone')
                archives.append(destination.read_bytes())
            self.assertTrue(all(archive == archives[0] for archive in archives), 'package metadata must be independent of the packaging timezone')

    def test_workload_packs_own_only_isoplane_assets_and_preserve_native_executables(self):
        for rid in ('linux-x64', 'osx-arm64', 'osx-x64'):
            with self.subTest(rid=rid):
                with tempfile.TemporaryDirectory(prefix='isoplane-workload-package-') as temporary:
                    root = Path(temporary)
                    source = root / 'toolchain'
                    files = ['cli/Isoplane.Cli.dll', 'bin/wasm-tools', 'dotnet/dotnet']
                    for name in files:
                        path = source / name
                        path.parent.mkdir(parents=True, exist_ok=True)
                        path.write_bytes(b'payload')
                    (source / 'bin/wasm-tools').chmod(0o755)
                    for name in ['Microsoft.NET.Runtime.MonoTargets.Sdk',
                                 f'Microsoft.NETCore.App.Runtime.AOT.{rid}.Cross.wasi-wasm']:
                        pack = source / 'dotnet/packs' / name / '10.0.11'
                        (pack / 'Sdk').mkdir(parents=True)
                        (pack / 'Sdk/Sdk.props').write_text('<Project />')
                        (pack / f'{name}.nuspec').write_text('upstream identity')
                        (pack / f'{name}.nupkg').write_bytes(b'upstream archive')
                    (source / 'toolchain.json').write_text(json.dumps({
                        'runtimeIdentifier': rid, 'wasiWorkloadVersion': '10.0.11',
                        'files': [{'path': name} for name in files],
                    }))
                    templates = root / 'templates.nupkg'
                    templates.write_bytes(b'templates')
                    output = root / 'release'
                    workload.package(source, templates, '1.2.3', output)
                    manifest = json.loads((output / 'WorkloadManifest.json').read_text())
                    for name, pack in manifest['packs'].items():
                        package_id = pack.get('alias-to', {}).get(rid, name)
                        self.assertTrue(package_id.startswith('Isoplane.'), 'workload ownership must not claim Microsoft pack identities')
                        self.assertTrue((output / 'packages' / f'{package_id}.1.2.3.nupkg').is_file(), 'every manifest pack must ship in the release')
                        if pack['kind'] == 'template':
                            continue
                        with zipfile.ZipFile(output / 'packages' / f'{package_id}.1.2.3.nupkg') as archive:
                            self.assertTrue(all(entry.date_time == (1980, 1, 1, 0, 0, 0) for entry in archive.infolist()),
                                            'generated and copied pack files must never inherit build-host timestamps')
                            nuspecs = [entry for entry in archive.namelist() if entry.endswith('.nuspec')]
                            self.assertEqual(len(nuspecs), 1, 'a repackaged SDK must carry only its own package identity')
                            identity = ET.fromstring(archive.read(nuspecs[0])).findtext('metadata/id')
                            self.assertEqual(identity, package_id, 'the NuGet identity must match workload installation records')
                            self.assertFalse(any(entry.startswith('dotnet/') for entry in archive.namelist()), 'the workload must not ship a replacement .NET installation')
                            if name == 'Isoplane.Toolchain':
                                self.assertTrue(archive.getinfo('bin/wasm-tools').external_attr >> 16 & 0o111, 'native tools must retain executable permissions')


if __name__ == '__main__':
    unittest.main()
