import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('package_debugger', Path(__file__).parents[1] / 'package-debugger.py')
package = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package)


class DebuggerPayloadTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='isoplane-debugger-test-')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        windows = package.platform.system() == 'Windows'
        extension = '.exe' if windows else ''
        self.files = ['bin/lldb' + extension, 'bin/lldb-dap' + extension, 'bin/lldb-server' + extension,
                      'bindings/lldb/__init__.py', 'python/python.exe' if windows else 'python/bin/python3']
        if package.platform.system() == 'Darwin':
            self.files.append('bin/debugserver')
        for name in self.files:
            file = self.root / name
            file.parent.mkdir(parents=True, exist_ok=True)
            file.write_bytes(b'payload')
        self.receipt = {'version': package.VERSION, 'runtimeIdentifier': package.runtime_identifier(),
                        'files': {name: hashlib.sha256(b'payload').hexdigest() for name in self.files}}
        self.write_receipt()

    def write_receipt(self):
        (self.root / 'debugger.json').write_text(json.dumps(self.receipt))

    def test_a_modified_debugger_binary_is_rejected(self):
        (self.root / self.files[1]).write_bytes(b'changed')
        with self.assertRaisesRegex(ValueError, 'differs from its receipt', msg='altered debugger binaries must not enter a release'):
            package.verify(self.root)

    def test_a_wrong_platform_payload_is_rejected(self):
        self.receipt['runtimeIdentifier'] = 'unsupported-platform'
        self.write_receipt()
        with self.assertRaisesRegex(ValueError, 'different host platform', msg='a release must carry a debugger for its own platform'):
            package.verify(self.root)

    def test_missing_bindings_are_rejected_even_when_removed_from_the_receipt(self):
        name = 'bindings/lldb/__init__.py'
        del self.receipt['files'][name]
        (self.root / name).unlink()
        self.write_receipt()
        with self.assertRaisesRegex(ValueError, 'incomplete', msg='a checksum receipt must not legitimize missing debugger bindings'):
            package.verify(self.root)

    def test_untracked_payload_files_are_rejected(self):
        (self.root / 'untracked').write_bytes(b'extra')
        with self.assertRaisesRegex(ValueError, 'inventory', msg='the release must account for every debugger file'):
            package.verify(self.root)

    def test_another_lldb_version_is_rejected(self):
        self.receipt['version'] = '0.0.0'
        self.write_receipt()
        with self.assertRaisesRegex(ValueError, 'expected LLDB', msg='the qualified LLDB version must remain pinned'):
            package.verify(self.root)

    def test_assembly_rejects_an_unqualified_executable_or_missing_python_bindings(self):
        (self.root / 'lib/clang').mkdir(parents=True)
        extension = '.exe' if package.platform.system() == 'Windows' else ''
        (self.root / ('bin/lldb-server' + extension)).write_bytes(b'payload')
        if package.platform.system() == 'Darwin':
            (self.root / 'bin/debugserver').write_bytes(b'payload')
        for replies, message in [(['lldb version 23.1.20'], 'expected LLDB'),
                                 ([f'lldb version {package.VERSION}', 'Python import failed'], 'bindings could not load')]:
            with self.subTest(failure=message), patch.object(package.subprocess, 'check_output', side_effect=replies):
                with self.assertRaisesRegex(ValueError, message, msg='a receipt must only describe an executable debugger with working Python'):
                    package.assemble(self.root, self.root / 'python', self.root / 'bindings', self.root / 'output')
                self.assertFalse((self.root / 'output').exists(), 'failed assembly must leave no publishable payload')

    def test_a_receipt_cannot_read_outside_its_payload(self):
        self.receipt['files']['../outside'] = '0' * 64
        self.write_receipt()
        with self.assertRaisesRegex(ValueError, 'unsafe debugger payload path', msg='receipt paths must stay inside the supplied debugger'):
            package.verify(self.root)

    def test_a_complete_payload_is_relocatable(self):
        destination = self.root / 'relocated'
        destination.mkdir()
        for path in list(self.root.iterdir()):
            if path != destination:
                path.rename(destination / path.name)
        self.assertEqual(package.verify(destination), self.receipt, 'receipts must contain no absolute staging paths')


if __name__ == '__main__':
    unittest.main()
