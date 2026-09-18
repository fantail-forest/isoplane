"""Release assembly rejects mixed identities, stale pins and corrupted cached outputs."""
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
import zipfile

spec = importlib.util.spec_from_file_location('build_preview', Path(__file__).parents[1] / 'build-preview.py')
preview = importlib.util.module_from_spec(spec)
spec.loader.exec_module(preview)


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.feed = self.root / 'packages'
        self.feed.mkdir()
        self.version = '0.1.0-preview.1'
        self.revision = '0123456789abcdef' * 2 + '01234567'
        self.library = self.root / preview.NATIVE_LIBRARY
        self.library.write_bytes(b'native support built for this release')
        for name in preview.PROJECTS:
            self.package(name)

    def package(self, name, version=None, revision=None, sdk_version=None, dependency_version=None, library=None):
        with zipfile.ZipFile(self.feed / f'{name}.{self.version}.nupkg', 'w') as archive:
            dependencies = ''
            if name in ('Isoplane.Cli', 'Isoplane.Testing'):
                dependencies = f'<dependencies><group><dependency id="Isoplane.Native" version="{dependency_version or self.version}" /></group></dependencies>'
            archive.writestr(name + '.nuspec', f'<package><metadata><id>{name}</id><version>{version or self.version}</version>'
                             f'<repository commit="{revision or self.revision}" />{dependencies}</metadata></package>')
            if name in ('Wisp.Sdk', 'Wisp.AspNetCore'):
                archive.writestr('Sdk/Sdk.props', f'<Project><ItemGroup><PackageReference Include="{name}" Version="{sdk_version or self.version}" /></ItemGroup></Project>')
            if name == 'Wisp.Templates':
                archive.writestr('content/content/aspnet/IsoplaneApp.csproj', f'<Project Sdk="Wisp.AspNetCore/{sdk_version or self.version}" />')
            if name == 'Isoplane.Cli':
                archive.writestr(f'tools/net10.0/any/runtime/{self.library.name}', library or self.library.read_bytes())
            if name == 'Isoplane.Native':
                archive.writestr(f'runtimes/{preview.RUNTIME_IDENTIFIER}/native/{self.library.name}', library or self.library.read_bytes())

    def verify(self):
        return preview.verify_packages(self.feed, self.version, self.revision, self.library)

    def test_a_coherent_feed_passes(self):
        self.assertIsNone(self.verify(), 'all release packages must agree')

    def test_a_renamed_stale_cli_is_rejected(self):
        self.package('Isoplane.Cli', revision='older')
        with self.assertRaisesRegex(ValueError, 'source revision differs'):
            self.verify()

    def test_a_different_embedded_version_is_rejected(self):
        self.package('Isoplane.Testing', version='0.1.0-old')
        with self.assertRaisesRegex(ValueError, 'package version'):
            self.verify()

    def test_a_stale_template_pin_is_rejected(self):
        self.package('Wisp.Templates', sdk_version='0.1.0-old')
        with self.assertRaisesRegex(ValueError, 'template selects'):
            self.verify()

    def test_a_stale_sdk_self_reference_is_rejected(self):
        self.package('Wisp.Sdk', sdk_version='0.1.0-old')
        with self.assertRaisesRegex(ValueError, 'SDK selects'):
            self.verify()

    def test_a_tool_depending_on_another_native_package_version_is_rejected(self):
        self.package('Isoplane.Testing', dependency_version='0.1.0-old')
        with self.assertRaisesRegex(ValueError, 'different release package'):
            self.verify()

    def test_a_native_package_carrying_another_library_is_rejected(self):
        self.package('Isoplane.Native', library=b'a library from another cargo build')
        with self.assertRaisesRegex(ValueError, 'native library other than'):
            self.verify()

    def test_a_cli_bundling_another_library_is_rejected(self):
        self.package('Isoplane.Cli', library=b'a library from another cargo build')
        with self.assertRaisesRegex(ValueError, 'native library differ'):
            self.verify()

    def test_missing_and_extra_packages_are_rejected(self):
        path = self.feed / f'Isoplane.Cli.{self.version}.nupkg'
        path.rename(self.feed / 'Isoplane.Cli.old.nupkg')
        with self.assertRaisesRegex(ValueError, f'exactly the {len(preview.PROJECTS)}'):
            self.verify()

    def test_modified_cached_payloads_are_rejected(self):
        receipt = preview.inventory(self.feed)
        next(self.feed.iterdir()).write_bytes(b'corrupted')
        with self.assertRaisesRegex(ValueError, 'content differs'):
            preview.verify_inventory(self.feed, receipt)

    def test_archive_bytes_do_not_depend_on_output_name_or_file_times(self):
        first, second = self.root / 'first.tgz', self.root / 'second.tgz'
        preview.archive_release(self.feed, first, 1234567890)
        for path in self.feed.iterdir():
            path.touch()
        preview.archive_release(self.feed, second, 1234567890)
        self.assertEqual(first.read_bytes(), second.read_bytes(), 'the same bundle must produce the same archive bytes')

    def test_export_preserves_runtime_builds_but_removes_managed_intermediates(self):
        repository = self.root / 'repository'
        repository.mkdir()
        subprocess.run(['git', 'init', '-q', repository], check=True)
        (repository / 'guest-runtime').mkdir()
        (repository / 'guest-runtime/manifest.env').write_text('pinned input')
        subprocess.run(['git', '-C', repository, 'add', '.'], check=True)
        subprocess.run(['git', '-C', repository, '-c', 'user.name=Test', '-c', 'user.email=test@example.invalid',
                        'commit', '-qm', 'fixture'], check=True)
        source = self.root / 'source'
        runtime = source / 'guest-runtime/out/feed'
        runtime.mkdir(parents=True)
        (runtime / 'pack.nupkg').write_bytes(b'verified later by runtime build')
        stale = source / 'tools/obj/old.dll'
        stale.parent.mkdir(parents=True)
        stale.write_bytes(b'stale managed build')
        preview.export_source(repository, source, self.root, 'HEAD', self.root)
        self.assertTrue((runtime / 'pack.nupkg').exists(), 'runtime outputs must survive a build retry')
        self.assertFalse(stale.exists(), 'the managed build must start without old intermediates')
        self.assertEqual((source / 'guest-runtime/manifest.env').read_text(), 'pinned input',
                         'the source snapshot must come from the selected revision')

    def test_uncommitted_sources_cannot_be_released(self):
        subprocess.run(['git', 'init', '-q', self.root], check=True)
        with self.assertRaisesRegex(ValueError, 'pending source changes'):
            preview.source_revision(self.root)

    def test_a_source_whose_pins_drifted_from_the_declared_version_cannot_be_released(self):
        source = self.root / 'source'
        (source / 'scripts').mkdir(parents=True)
        (source / 'scripts/release-version.py').write_bytes((Path(__file__).parents[1] / 'release-version.py').read_bytes())
        (source / 'Directory.Build.props').write_text('<Project><PropertyGroup><Version>0.1.0-preview.2</Version></PropertyGroup></Project>')
        (source / 'App.csproj').write_text('<Project Sdk="Wisp.AspNetCore/0.1.0-preview.1" />')
        with self.assertRaisesRegex(ValueError, 'App.csproj pins 0.1.0-preview.1'):
            preview.release_version(source)


class ExistingReleaseTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.release = self.root / preview.release_name('0.1.0-preview.1')
        self.archive = self.release.with_name(self.release.name + '.tgz')
        self.receipt = self.root / 'receipt.json'
        self.revision = 'a' * 40

    def assemble(self, revision):
        self.release.mkdir()
        (self.release / 'release.json').write_text(json.dumps({'sourceRevision': revision}))
        preview.archive_release(self.release, self.archive, 1234567890)
        checksum = preview.sha(self.archive)
        self.archive.with_name(self.archive.name + '.sha256').write_text(f'{checksum}  {self.archive.name}\n')
        self.receipt.write_text(json.dumps({'sourceRevision': revision, 'files': preview.inventory(self.release), 'archiveSha256': checksum}))

    def test_nothing_assembled_yet_proceeds_to_a_build(self):
        self.assertFalse(preview.existing_release(self.release, self.archive, self.receipt, self.revision))

    def test_the_same_revision_reuses_its_verified_release(self):
        self.assemble(self.revision)
        self.assertTrue(preview.existing_release(self.release, self.archive, self.receipt, self.revision),
                        'rebuilding one revision at one version is idempotent')

    def test_another_revision_cannot_reuse_a_released_version(self):
        self.assemble('b' * 40)
        with self.assertRaisesRegex(ValueError, 'already assembled from revision bbbbbbbbbbbb'):
            preview.existing_release(self.release, self.archive, self.receipt, self.revision)

    def test_a_release_without_a_receipt_is_not_trusted(self):
        self.release.mkdir()
        with self.assertRaisesRegex(ValueError, 'unreceipted release'):
            preview.existing_release(self.release, self.archive, self.receipt, self.revision)

    def test_a_release_altered_after_its_receipt_is_rejected(self):
        self.assemble(self.revision)
        (self.release / 'release.json').write_text(json.dumps({'sourceRevision': self.revision, 'altered': True}))
        with self.assertRaisesRegex(ValueError, 'content differs'):
            preview.existing_release(self.release, self.archive, self.receipt, self.revision)


if __name__ == '__main__':
    unittest.main()
