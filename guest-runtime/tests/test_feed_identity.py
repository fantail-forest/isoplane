import base64
import hashlib
from pathlib import Path
import tempfile
import zipfile
import unittest
import xml.etree.ElementTree as ET

import importlib.util
spec = importlib.util.spec_from_file_location('feed_identity', Path(__file__).resolve().parents[1] / 'feed-identity.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
input_digest, verify_feed = module.input_digest, module.verify_feed


class FeedTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='wisp-feed-identity-')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        for name in ['manifest.env', 'build.sh', 'source-tree.py', 'feed-identity.py']:
            (self.root / name).write_text(name)
        (self.root / 'patches').mkdir()
        (self.root / 'patches/one.patch').write_text('source')
        self.feed = self.root / 'feed'
        self.feed.mkdir()
        props = ET.Element('Project')
        group = ET.SubElement(props, 'PropertyGroup')
        ET.SubElement(group, 'WispGuestRuntimeInputsSha256').text = input_digest(self.root)
        for package, prefix in [
            ('Microsoft.NETCore.App.Runtime.Mono.wasi-wasm', 'WispGuestRuntimePack'),
            ('Microsoft.NET.Runtime.WebAssembly.Wasi.Sdk', 'WispGuestWasiSdkPack'),
            ('runtime.wasi-wasm.Microsoft.DotNet.ILCompiler.LLVM', 'WispGuestNativeAotPack'),
        ]:
            content = package.encode()
            path = self.feed / f'{package}.1.nupkg'
            with zipfile.ZipFile(path, 'w') as archive:
                archive.writestr('lib/library.dll', content)
            directory = self.root / 'packs' / package
            (directory / 'lib').mkdir(parents=True)
            (directory / 'lib/library.dll').write_bytes(content)
            ET.SubElement(group, prefix + 'Directory').text = '$(MSBuildThisFileDirectory)../packs/' + package
            content = path.read_bytes()
            ET.SubElement(group, prefix + 'Version').text = '1'
            ET.SubElement(group, prefix + 'Sha512').text = base64.b64encode(hashlib.sha512(content).digest()).decode()
        ET.ElementTree(props).write(self.feed / 'Wisp.GuestRuntime.props')

    def test_current_published_inputs_are_accepted(self):
        self.assertEqual(verify_feed(self.feed, self.root), input_digest(self.root))

    def test_source_edit_refuses_default_feed(self):
        (self.root / 'patches/one.patch').write_text('changed')
        with self.assertRaisesRegex(ValueError, 'stale'):
            verify_feed(self.feed, self.root)

    def test_explicit_artifact_accepts_different_current_sources(self):
        (self.root / 'patches/one.patch').write_text('changed')
        self.assertNotEqual(verify_feed(self.feed), input_digest(self.root))

    def test_every_pack_must_match_including_the_wasi_sdk(self):
        for path in self.feed.glob('*.nupkg'):
            with self.subTest(package=path.name):
                original = path.read_bytes()
                path.write_bytes(original + b'changed')
                with self.assertRaisesRegex(ValueError, 'content differs'):
                    verify_feed(self.feed)
                path.write_bytes(original)

    def test_unpacked_runtime_changes_are_refused(self):
        library = next((self.root / 'packs').glob('*/lib/library.dll'))
        library.write_bytes(b'changed')
        with self.assertRaisesRegex(ValueError, 'unpacked runtime package content differs'):
            verify_feed(self.feed, self.root)

    def test_extra_unpacked_assemblies_are_refused(self):
        directory = next((self.root / 'packs').iterdir())
        (directory / 'lib/injected.dll').write_bytes(b'extra')
        with self.assertRaisesRegex(ValueError, 'inventory differs'):
            verify_feed(self.feed)

    def test_unpacked_directory_symlinks_are_refused(self):
        directory = next((self.root / 'packs').iterdir())
        (directory / 'alias').symlink_to(directory / 'lib', target_is_directory=True)
        with self.assertRaisesRegex(ValueError, 'contains a link'):
            verify_feed(self.feed)

    def test_feed_and_packs_remain_valid_after_relocation(self):
        import shutil
        with tempfile.TemporaryDirectory(prefix='wisp-relocated-feed-') as temporary:
            relocated = Path(temporary)
            shutil.copytree(self.feed, relocated / 'feed')
            shutil.copytree(self.root / 'packs', relocated / 'packs')
            self.assertEqual(verify_feed(relocated / 'feed'), input_digest(self.root))

    def test_patch_rename_changes_identity(self):
        original = input_digest(self.root)
        (self.root / 'patches/one.patch').rename(self.root / 'patches/two.patch')
        self.assertNotEqual(original, input_digest(self.root))

    def test_source_timestamp_changes_do_not_change_identity(self):
        original = input_digest(self.root)
        (self.root / 'patches/one.patch').touch()
        self.assertEqual(original, input_digest(self.root))

    def test_explicit_artifact_refuses_malformed_input_identity(self):
        path = self.feed / 'Wisp.GuestRuntime.props'
        props = ET.parse(path)
        props.getroot().find('./PropertyGroup/WispGuestRuntimeInputsSha256').text = 'not-a-digest'
        props.write(path)
        with self.assertRaisesRegex(ValueError, 'not a lowercase SHA256'):
            verify_feed(self.feed)

    def test_duplicate_properties_cannot_disagree_with_msbuild(self):
        path = self.feed / 'Wisp.GuestRuntime.props'
        props = ET.parse(path)
        ET.SubElement(props.getroot().find('PropertyGroup'), 'WispGuestRuntimePackVersion').text = '2'
        props.write(path)
        with self.assertRaisesRegex(ValueError, 'ambiguous duplicate'):
            verify_feed(self.feed)

    def test_missing_provenance_is_refused(self):
        path = self.feed / 'Wisp.GuestRuntime.props'
        props = ET.parse(path)
        group = props.getroot().find('PropertyGroup')
        group.remove(group.find('WispGuestRuntimeInputsSha256'))
        props.write(path)
        with self.assertRaisesRegex(ValueError, 'no WispGuestRuntimeInputsSha256'):
            verify_feed(self.feed, self.root)


if __name__ == '__main__':
    unittest.main()
