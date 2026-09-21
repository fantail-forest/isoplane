import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
import xml.etree.ElementTree as ET
import zipfile

spec = importlib.util.spec_from_file_location('package_workload', Path(__file__).parents[1] / 'package-workload.py')
workload = importlib.util.module_from_spec(spec)
spec.loader.exec_module(workload)


class WorkloadPackageTests(unittest.TestCase):
    def test_workload_packs_own_only_isoplane_assets_and_preserve_native_executables(self):
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
                         'Microsoft.NETCore.App.Runtime.AOT.linux-x64.Cross.wasi-wasm']:
                pack = source / 'dotnet/packs' / name / '10.0.11'
                (pack / 'Sdk').mkdir(parents=True)
                (pack / 'Sdk/Sdk.props').write_text('<Project />')
                (pack / f'{name}.nuspec').write_text('upstream identity')
                (pack / f'{name}.nupkg').write_bytes(b'upstream archive')
            (source / 'toolchain.json').write_text(json.dumps({
                'runtimeIdentifier': 'linux-x64', 'wasiWorkloadVersion': '10.0.11',
                'files': [{'path': name} for name in files],
            }))
            templates = root / 'templates.nupkg'
            templates.write_bytes(b'templates')
            output = root / 'release'
            workload.package(source, templates, '1.2.3', output)
            manifest = json.loads((output / 'WorkloadManifest.json').read_text())
            for name, pack in manifest['packs'].items():
                package_id = pack.get('alias-to', {}).get('linux-x64', name)
                self.assertTrue(package_id.startswith('Isoplane.'), 'workload ownership must not claim Microsoft pack identities')
                self.assertTrue((output / 'packages' / f'{package_id}.1.2.3.nupkg').is_file(), 'every manifest pack must ship in the release')
                if pack['kind'] == 'template':
                    continue
                with zipfile.ZipFile(output / 'packages' / f'{package_id}.1.2.3.nupkg') as archive:
                    nuspecs = [entry for entry in archive.namelist() if entry.endswith('.nuspec')]
                    self.assertEqual(len(nuspecs), 1, 'a repackaged SDK must carry only its own package identity')
                    identity = ET.fromstring(archive.read(nuspecs[0])).findtext('metadata/id')
                    self.assertEqual(identity, package_id, 'the NuGet identity must match workload installation records')
                    self.assertFalse(any(entry.startswith('dotnet/') for entry in archive.namelist()), 'the workload must not ship a replacement .NET installation')
                    if name == 'Isoplane.Toolchain':
                        self.assertTrue(archive.getinfo('bin/wasm-tools').external_attr >> 16 & 0o111, 'native tools must retain executable permissions')


if __name__ == '__main__':
    unittest.main()
