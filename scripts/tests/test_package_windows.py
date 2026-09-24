import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
import xml.etree.ElementTree as ET

spec = importlib.util.spec_from_file_location('package_windows', Path(__file__).parents[1] / 'package-windows.py')
windows = importlib.util.module_from_spec(spec)
spec.loader.exec_module(windows)


class WindowsPackageTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='isoplane-msi-')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.files = ['Sdk/Sdk.props', 'feed/Isoplane.GuestRuntime.props', 'compiler/Isoplane.GuestCompiler.props',
                      'cli/Isoplane.Cli.dll', 'cli/runtime/isoplane_native.dll', 'bin/wasm-tools.exe',
                      'wasi-sdk/bin/clang.exe', 'mono-targets/Sdk/Sdk.props', 'wasi-cross/Sdk/Sdk.props',
                      'templates/Isoplane.Templates.1.2.3-preview.1.nupkg']
        self.manifest = {'runtimeIdentifier': 'win-x64', 'files': []}
        for name in self.files:
            file = self.root / name
            file.parent.mkdir(parents=True, exist_ok=True)
            file.write_bytes(b'payload')
            self.manifest['files'].append({'path': name, 'sha256': hashlib.sha256(b'payload').hexdigest()})

    def author(self):
        (self.root / 'toolchain.json').write_text(json.dumps(self.manifest))
        output = self.root / 'sdk.wxs'
        windows.author(self.root, '1.2.3-preview.1', output)
        return ET.parse(output)

    def test_msi_owns_only_versioned_isoplane_files(self):
        tree = self.author()
        namespace = {'w': windows.NAMESPACE}
        files = tree.findall('.//w:File', namespace)
        self.assertEqual(len(files), len(self.files) + 1, 'MSI must own all verified payload files and their integrity manifest')
        self.assertFalse(tree.findall('.//w:RegistryValue', namespace), 'MSI must not register .NET workload ownership')
        directories = [entry.get('Name') for entry in tree.findall('.//w:Directory', namespace)]
        self.assertIn('1.2.3-preview.1', directories, 'SDK releases must install side by side')
        self.assertNotIn('sdk-manifests', directories, 'MSI must not advertise Isoplane to workload maintenance')

    def test_workload_registration_payload_is_rejected(self):
        self.manifest['files'].append({'path': 'sdk-manifests/10.0.400/isoplane/WorkloadManifest.json', 'sha256': ''})
        with self.assertRaisesRegex(ValueError, 'workload state'):
            self.author()

    def test_changed_payload_is_rejected_before_msi_generation(self):
        (self.root / 'bin/wasm-tools.exe').write_bytes(b'altered')
        with self.assertRaisesRegex(ValueError, 'integrity check failed'):
            self.author()


if __name__ == '__main__':
    unittest.main()
