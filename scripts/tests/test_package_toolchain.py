import importlib.util
from pathlib import Path
import tempfile
import unittest
import zipfile

spec = importlib.util.spec_from_file_location('package_toolchain', Path(__file__).parents[1] / 'package-toolchain.py')
package_toolchain = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package_toolchain)


class NativePairTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='isoplane-package-pair-')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.runtime = self.root / 'libisoplane_native.so'
        self.runtime.write_bytes(b'engine payload')
        self.package = self.root / 'cli.nupkg'
        with zipfile.ZipFile(self.package, 'w') as archive:
            for path in [self.runtime]:
                archive.write(path, 'tools/net10.0/any/runtime/' + path.name)

    def test_the_selected_pair_matches_the_actual_cli_archive(self):
        self.assertIsNone(package_toolchain.verify_native_pair(self.package, self.runtime),
                          'a matching CLI/native pair must pass validation')

    def test_a_new_binary_cannot_be_combined_with_an_older_cli_package(self):
        for path in [self.runtime]:
            with self.subTest(binary=path.name):
                original = path.read_bytes()
                path.write_bytes(b'another build')
                with self.assertRaisesRegex(ValueError, 'library differ'):
                    package_toolchain.verify_native_pair(self.package, self.runtime)
                path.write_bytes(original)

    def test_an_incomplete_cli_archive_is_rejected(self):
        with zipfile.ZipFile(self.package, 'w') as archive:
            archive.writestr('tools/net10.0/any/Isoplane.Cli.dll', b'managed')
        with self.assertRaisesRegex(ValueError, 'exactly one runtime/libisoplane_native.so'):
            package_toolchain.verify_native_pair(self.package, self.runtime)

    def test_cli_extraction_rejects_paths_outside_its_payload(self):
        with zipfile.ZipFile(self.package, 'w') as archive:
            archive.writestr('tools/net10.0/any/../escape', b'invalid')
        with self.assertRaisesRegex(ValueError, 'Unsafe CLI package path'):
            package_toolchain.extract_cli(self.package, self.root / 'extracted')
        self.assertFalse((self.root / 'escape').exists(), 'archive traversal must not create files outside the CLI payload')


class ReadabilityTests(unittest.TestCase):
    def test_private_build_staging_modes_become_readable_by_every_user(self):
        with tempfile.TemporaryDirectory(prefix='isoplane-package-modes-') as temporary:
            stage = Path(temporary) / 'toolchain'
            private = stage / 'compiler'
            private.mkdir(parents=True)
            private.chmod(0o700)
            (private / 'props').write_text('settings')
            (private / 'props').chmod(0o600)
            (private / 'tool').write_text('#!/bin/sh')
            (private / 'tool').chmod(0o700)
            package_toolchain.make_world_readable(stage)
            self.assertEqual(private.stat().st_mode & 0o777, 0o755, 'a customer installing as another user must be able to enter every release directory')
            self.assertEqual((private / 'props').stat().st_mode & 0o777, 0o644, 'a customer must be able to read every release file')
            self.assertEqual((private / 'tool').stat().st_mode & 0o777, 0o755, 'executables stay executable for every user')


if __name__ == '__main__':
    unittest.main()
