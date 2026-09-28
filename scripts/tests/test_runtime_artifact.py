"""Runtime handoffs reject corruption, external paths and unsafe archive entries."""
import base64
import hashlib
import importlib.util
import io
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest import mock
import xml.etree.ElementTree as ET
import zipfile

spec = importlib.util.spec_from_file_location('runtime_artifact', Path(__file__).parents[1] / 'runtime-artifact.py')
artifact = importlib.util.module_from_spec(spec)
spec.loader.exec_module(artifact)


class RuntimeArtifactTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.output = self.root / 'runtime'
        feed = self.output / 'feed'
        feed.mkdir(parents=True)
        props = ET.Element('Project')
        group = ET.SubElement(props, 'PropertyGroup')
        ET.SubElement(group, 'IsoplaneGuestRuntimeInputsSha256').text = artifact.identity_module().input_digest(artifact.ROOT / 'guest-runtime')
        for name, prefix, directory in [
            ('Microsoft.NETCore.App.Runtime.Mono.wasi-wasm', 'IsoplaneGuestRuntimePack', 'runtime'),
            ('Microsoft.NET.Runtime.WebAssembly.Wasi.Sdk', 'IsoplaneGuestWasiSdkPack', 'wasi-sdk'),
        ]:
            path = feed / f'{name}.1.nupkg'
            with zipfile.ZipFile(path, 'w') as package:
                package.writestr('library.dll', name.encode())
                package.extractall(self.output / 'packs' / directory)
            ET.SubElement(group, prefix + 'Version').text = '1'
            ET.SubElement(group, prefix + 'Directory').text = f'$(MSBuildThisFileDirectory)../packs/{directory}/'
            ET.SubElement(group, prefix + 'Sha512').text = base64.b64encode(hashlib.sha512(path.read_bytes()).digest()).decode()
        ET.ElementTree(props).write(feed / 'Isoplane.GuestRuntime.props')
        self.archive = self.root / 'runtime.tgz'

    def test_exported_runtime_can_be_imported_to_another_directory(self):
        digest = artifact.export_artifact(self.output, self.archive)
        destination = self.root / 'imported'
        artifact.import_artifact(str(self.archive), digest, destination)
        self.assertEqual(artifact.verify(destination), artifact.verify(self.output), 'handoff must preserve runtime source identity')

    def test_failed_export_can_be_retried(self):
        with mock.patch.object(tarfile.TarFile, 'add', side_effect=OSError('disk full')):
            with self.assertRaisesRegex(OSError, 'disk full'):
                artifact.export_artifact(self.output, self.archive)
        self.assertFalse(self.archive.exists(), 'a failed export must not leave a reusable archive')
        digest = artifact.export_artifact(self.output, self.archive)
        self.assertEqual(artifact.checksum(self.archive), digest, 'retry must produce a complete archive')

    def test_corrupted_download_does_not_create_destination(self):
        digest = artifact.export_artifact(self.output, self.archive)
        self.archive.write_bytes(self.archive.read_bytes() + b'corrupt')
        destination = self.root / 'imported'
        with self.assertRaisesRegex(ValueError, 'SHA256 differs'):
            artifact.import_artifact(str(self.archive), digest, destination)
        self.assertFalse(destination.exists(), 'a rejected archive must leave no usable output')

    def test_pack_directory_cannot_escape_the_artifact(self):
        path = self.output / 'feed/Isoplane.GuestRuntime.props'
        path.write_text(path.read_text().replace('../packs/runtime/', '../../outside/'))
        with self.assertRaisesRegex(ValueError, 'external pack directory'):
            artifact.export_artifact(self.output, self.archive)

    def test_unsafe_archive_entries_are_rejected_before_extraction(self):
        for name in ['../escape', 'packs/../../escape', 'C:/escape', 'packs/file:stream', 'packs/directory./file']:
            with self.subTest(name=name):
                with tarfile.open(self.archive, 'w:gz') as bundle:
                    entry = tarfile.TarInfo(name)
                    entry.size = 1
                    bundle.addfile(entry, io.BytesIO(b'x'))
                with self.assertRaisesRegex(ValueError, 'unsafe runtime artifact entry'):
                    artifact.extract(self.archive, self.root / 'unpacked')

    def test_foreign_source_identity_is_rejected(self):
        path = self.output / 'feed/Isoplane.GuestRuntime.props'
        original = artifact.verify(self.output)
        path.write_text(path.read_text().replace(original, '0' * 64))
        with self.assertRaisesRegex(ValueError, 'stale'):
            artifact.export_artifact(self.output, self.archive)


if __name__ == '__main__':
    unittest.main()
