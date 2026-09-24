"""Remote packing rejects runtime modules from another source, version or Mono feed."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


def load(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).parents[1] / (name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


modules = load('shared-runtime-modules')


class SharedModulesTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)
        self.version, self.revision = '0.1.0-preview.3', 'a' * 40
        self.pack = '10.0.0-test@sha512:testdigest'
        self.receipt = {'sdkVersion': self.version, 'sourceRevision': self.revision, 'monoPack': self.pack, 'modules': {}}
        for name in modules.module_names():
            (self.directory / name).write_bytes(name.encode())
            (self.directory / (name + '.runtime')).write_text(self.pack)
            self.receipt['modules'][name] = hashlib.sha256(name.encode()).hexdigest()
        self.write_receipt()
        self.feed = self.directory / 'feed'
        self.feed.mkdir()
        (self.feed / 'Isoplane.GuestRuntime.props').write_text('<Project><PropertyGroup><IsoplaneGuestRuntimePackVersion>10.0.0-test</IsoplaneGuestRuntimePackVersion><IsoplaneGuestRuntimePackSha512>testdigest</IsoplaneGuestRuntimePackSha512></PropertyGroup></Project>')

    def write_receipt(self):
        (self.directory / 'runtime-build.json').write_text(json.dumps(self.receipt))

    def verify(self):
        return modules.verify(self.directory, self.version, self.revision, self.feed)

    def fixture_runtime(self, selection=None):
        packages = load('fixture-packages')
        source = self.directory / 'source'
        source.mkdir(exist_ok=True)
        (source / 'Directory.Build.props').write_text(f'<Project><PropertyGroup><Version>{self.version}</Version></PropertyGroup></Project>')
        with patch.dict(os.environ, {'ISOPLANE_TEST_RUNTIME_MODULES': str(selection or self.directory),
                                    'ISOPLANE_TEST_RUNTIME_FEED': str(self.feed)}), \
                patch.object(packages.subprocess, 'run', side_effect=AssertionError('explicit runtime artifacts must not invoke a native build')):
            return packages.shared_runtime(source)

    def test_fixture_consumers_accept_verified_portable_runtime_artifacts(self):
        self.assertEqual(self.fixture_runtime(), self.directory,
                         'fixture packages must consume the selected verified runtime without rebuilding it')

    def test_fixture_consumers_reject_changed_runtime_artifacts(self):
        (self.directory / modules.module_names()[0]).write_bytes(b'changed')
        with self.assertRaisesRegex(ValueError, 'SHA256'):
            self.fixture_runtime()

    def test_fixture_consumers_reject_another_sdk_version(self):
        self.receipt['sdkVersion'] = 'different'
        self.write_receipt()
        with self.assertRaisesRegex(ValueError, 'SDK version'):
            self.fixture_runtime()

    def test_fixture_consumers_require_an_absolute_artifact_selection(self):
        with self.assertRaisesRegex(ValueError, 'absolute artifact directory'):
            self.fixture_runtime(Path('relative'))

    def test_matching_inputs_are_portable_without_rebuilding(self):
        self.assertEqual(self.verify(), self.receipt, 'matching inputs must retain their recorded identity')
        self.assertTrue(all(path.is_file() for path in modules.files(self.directory)), 'handoff must include modules, sidecars and receipt')

    def test_another_sdk_version_is_rejected(self):
        self.receipt['sdkVersion'] = '0.1.0-preview.2'
        self.write_receipt()
        with self.assertRaisesRegex(ValueError, 'SDK version'):
            self.verify()

    def test_another_revision_is_rejected(self):
        self.receipt['sourceRevision'] = 'b' * 40
        self.write_receipt()
        with self.assertRaisesRegex(ValueError, 'source revision'):
            self.verify()

    def test_changed_module_is_rejected(self):
        (self.directory / modules.module_names()[0]).write_bytes(b'changed')
        with self.assertRaisesRegex(ValueError, 'SHA256'):
            self.verify()

    def test_changed_sidecar_is_rejected(self):
        (self.directory / (modules.module_names()[0] + '.runtime')).write_text('different')
        with self.assertRaisesRegex(ValueError, 'sidecar'):
            self.verify()

    def test_different_mono_feed_is_rejected(self):
        self.receipt['monoPack'] = 'another@sha512:pack'
        self.write_receipt()
        with self.assertRaisesRegex(ValueError, 'selected feed'):
            self.verify()

    def test_missing_module_inventory_is_rejected(self):
        self.receipt['modules'].pop(modules.module_names()[0])
        self.write_receipt()
        with self.assertRaisesRegex(ValueError, 'inventory'):
            self.verify()

    def test_empty_module_is_rejected(self):
        (self.directory / modules.module_names()[0]).write_bytes(b'')
        with self.assertRaisesRegex(ValueError, 'nonempty'):
            self.verify()


