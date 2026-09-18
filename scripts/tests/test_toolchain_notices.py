"""Managed attribution follows the actual restored package inventory."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('notices', Path(__file__).parents[1] / 'toolchain-notices.py')
notices = importlib.util.module_from_spec(spec)
spec.loader.exec_module(notices)


class ManagedNoticesTests(unittest.TestCase):
    def test_the_restored_license_file_is_included(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            packages = root / 'cache'
            package = packages / 'dependency/1.0.0'
            package.mkdir(parents=True)
            (package / 'dependency.nuspec').write_text('<package><metadata><license type="file">LICENSE</license></metadata></package>')
            (package / 'LICENSE').write_text('the upstream license')
            assets = root / 'tools/Example/obj/project.assets.json'
            assets.parent.mkdir(parents=True)
            assets.write_text(json.dumps({'packageFolders': {str(packages): {}},
                                          'libraries': {'Dependency/1.0.0': {'type': 'package', 'path': 'dependency/1.0.0'}}}))
            result = root / 'notices'
            notices.managed_notices(root, result)
            self.assertEqual((result / 'managed/Dependency/1.0.0/LICENSE').read_text(), 'the upstream license',
                             'the actual restored package license must be redistributed')

    def test_an_expression_includes_its_license_text(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            package = root / 'cache/dependency/1.0.0'
            package.mkdir(parents=True)
            (package / 'dependency.nuspec').write_text('<package><metadata><license type="expression">MIT</license></metadata></package>')
            assets = root / 'guest-sdks/dotnet/obj/project.assets.json'
            assets.parent.mkdir(parents=True)
            assets.write_text(json.dumps({'packageFolders': {str(root / 'cache'): {}},
                                          'libraries': {'Dependency/1.0.0': {'type': 'package', 'path': 'dependency/1.0.0'}}}))
            with patch.object(notices, 'fetch', return_value=b'license text') as fetch:
                notices.managed_notices(root, root / 'notices')
            fetch.assert_called_once_with('https://licenses.nuget.org/MIT')
            self.assertEqual((root / 'notices/managed/Dependency/1.0.0/LICENSE.html').read_bytes(), b'license text',
                             'an SPDX expression must include the referenced license text')


class GitSourceLicenseTests(unittest.TestCase):
    def test_a_workspace_member_of_a_git_dependency_takes_the_checkout_license(self):
        with tempfile.TemporaryDirectory() as temporary:
            checkout = Path(temporary) / 'checkouts/fork-abc/123456'
            member = checkout / 'cranelift/assembler'
            member.mkdir(parents=True)
            (checkout / '.cargo-ok').write_text('')
            (checkout / 'LICENSE').write_text('workspace license')
            (member / 'Cargo.toml').write_text('[package]')
            package = {'name': 'assembler', 'version': '1.0.0', 'manifest_path': str(member / 'Cargo.toml'),
                       'source': 'git+ssh://git@example.invalid/fork?rev=123456#123456'}
            self.assertEqual(notices.source_licenses(package, {}, {}), {'LICENSE': b'workspace license'},
                             'a crate without its own license file is covered by the license at the root of its git checkout')

    def test_a_registry_crate_without_license_texts_is_still_reported(self):
        with tempfile.TemporaryDirectory() as temporary:
            crate = Path(temporary) / 'registry/src/index/bare-1.0.0'
            crate.mkdir(parents=True)
            (crate / 'Cargo.toml').write_text('[package]')
            package = {'name': 'bare', 'version': '1.0.0', 'manifest_path': str(crate / 'Cargo.toml'),
                       'source': 'registry+https://github.com/rust-lang/crates.io-index'}
            with self.assertRaisesRegex(ValueError, 'No packaged license texts for bare 1.0.0'):
                notices.source_licenses(package, {}, {})


if __name__ == '__main__':
    unittest.main()
