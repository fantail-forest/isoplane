"""Common-feed assembly from immutable platform release inputs."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
import zipfile

spec = importlib.util.spec_from_file_location('assembly', Path(__file__).parents[1] / 'assemble-publication.py')
assembly = importlib.util.module_from_spec(spec)
spec.loader.exec_module(assembly)


class AssemblyTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.version = '0.1.0-preview.3'
        self.revision = 'a' * 40
        self.releases = [self.release(rid) for rid in assembly.publication.PLATFORMS]
        self.output = self.root / 'assembled'

    def release(self, rid):
        directory = self.root / f'isoplane-{rid}-{self.version}'
        packages = directory / 'packages'
        packages.mkdir(parents=True)
        library = rid.encode()
        name = assembly.native_name(rid)
        records = []
        for package_id in assembly.preview.PROJECTS:
            path = packages / f'{package_id}.{self.version}.nupkg'
            with zipfile.ZipFile(path, 'w') as archive:
                archive.writestr('[Content_Types].xml', '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="nuspec" ContentType="application/octet" /></Types>')
                archive.writestr(package_id + '.nuspec', f'<package><metadata><id>{package_id}</id><version>{self.version}</version><repository commit="{self.revision}" /></metadata></package>')
                archive.writestr('README.md', rid)
                if package_id in ('Isoplane.Sdk', 'Isoplane.AspNetCore'):
                    archive.writestr('Sdk/Sdk.props', f'<Project><PropertyGroup><IsoplaneSdkVersion>{self.version}</IsoplaneSdkVersion></PropertyGroup><ItemGroup><PackageReference Include="{package_id}" Version="{self.version}" /></ItemGroup></Project>')
                if package_id == 'Isoplane.Templates':
                    archive.writestr('content/content/aspnet/SampleApp.csproj', f'<Project Sdk="Isoplane.AspNetCore/{self.version}" />')
                if package_id == 'Isoplane.Native':
                    archive.writestr(f'runtimes/{rid}/native/{name}', library)
                if package_id == 'Isoplane.Cli':
                    archive.writestr(f'tools/net10.0/any/runtime/{name}', library)
            records.append({'id': package_id, 'version': self.version, 'file': f'packages/{path.name}', 'sha256': assembly.preview.sha(path)})
        (directory / 'toolchain.tgz').write_bytes(b'toolchain')
        manifest = {'runtimeIdentifier': rid, 'packageVersion': self.version, 'sourceRevision': self.revision,
                    'packages': records, 'toolchain': {'archive': 'toolchain.tgz', 'archiveSha256': assembly.preview.sha(directory / 'toolchain.tgz')}}
        (directory / 'release.json').write_text(json.dumps(manifest))
        return directory

    def test_each_package_contains_all_platform_native_libraries(self):
        assembly.assemble(self.releases, self.output)
        for package_id in ('Isoplane.Native', 'Isoplane.Cli'):
            path = self.output / 'nuget' / self.version / f'{package_id}.{self.version}.nupkg'
            with zipfile.ZipFile(path) as archive:
                for rid in assembly.publication.PLATFORMS:
                    prefix = f'runtimes/{rid}/native/' if package_id == 'Isoplane.Native' else 'tools/net10.0/any/runtime/'
                    self.assertEqual(archive.read(prefix + assembly.native_name(rid)), rid.encode(), 'each platform must retain its verified native bytes')
                self.assertEqual(archive.read('README.md'), b'linux-x64', 'Linux supplies the canonical shared package contents')

    def test_assembly_is_reproducible(self):
        assembly.assemble(self.releases, self.output)
        other = self.root / 'second'
        assembly.assemble(list(reversed(self.releases)), other)
        for path in (self.output / 'nuget').rglob('*.nupkg'):
            self.assertEqual(path.read_bytes(), (other / path.relative_to(self.output)).read_bytes(), 'input ordering must not change package bytes')

    def test_upload_plan_publishes_manifests_after_payloads(self):
        assembly.assemble(self.releases, self.output)
        plan = json.loads((self.output / 'upload-plan.json').read_text())
        self.assertEqual(plan[-1]['key'], f'publication/{self.version}/manifest.json', 'publication manifest is the final commit marker')
        for operation in plan:
            self.assertEqual(assembly.preview.sha(Path(operation['file'])), operation['sha256'], 'every plan entry must pin actual upload bytes')
        first_manifest = next(i for i, entry in enumerate(plan) if entry['key'].endswith('/release.json'))
        self.assertTrue(all(entry['key'].endswith('.json') for entry in plan[first_manifest:]), 'all payloads must precede release manifests')

    def test_corrupt_toolchain_is_rejected_without_output(self):
        (self.releases[0] / 'toolchain.tgz').write_bytes(b'changed')
        with self.assertRaisesRegex(ValueError, 'toolchain SHA256'):
            assembly.assemble(self.releases, self.output)
        self.assertFalse(self.output.exists(), 'invalid input must not expose publication output')

    def test_corrupt_package_is_rejected(self):
        path = next((self.releases[0] / 'packages').glob('*.nupkg'))
        path.write_bytes(b'changed')
        with self.assertRaisesRegex(ValueError, 'package SHA256'):
            assembly.assemble(self.releases, self.output)

    def test_incomplete_platform_set_is_rejected(self):
        with self.assertRaisesRegex(ValueError, 'all supported'):
            assembly.assemble(self.releases[:2], self.output)

    def test_duplicate_platform_is_rejected(self):
        with self.assertRaisesRegex(ValueError, 'duplicate platform'):
            assembly.assemble([*self.releases, self.releases[0]], self.output)

    def test_existing_output_is_never_overwritten(self):
        self.output.mkdir()
        with self.assertRaisesRegex(ValueError, 'already exists'):
            assembly.assemble(self.releases, self.output)

    def test_assembled_feed_passes_publication_verification(self):
        metadata = assembly.assemble(self.releases, self.output)
        plan = json.loads((self.output / 'upload-plan.json').read_text())
        objects = {entry['key']: Path(entry['file']) for entry in plan}

        class Downloads:
            def fetch(self, key, output, limit):
                data = objects[key].read_bytes()
                if len(data) > limit:
                    raise ValueError('download limit')
                output.write(data)

            def exists(self, key):
                if not objects[key].is_file():
                    raise ValueError('missing artifact')

        verified = self.root / 'verified'
        assembly.publication.verify(self.version, assembly.preview.sha(self.output / metadata), verified, Downloads())
        packages = json.loads((verified / 'packages.json').read_text())
        self.assertEqual(len(packages), len(assembly.preview.PROJECTS), 'every assembled package must pass the publication gate')

    def test_mixed_source_revisions_are_rejected(self):
        self.revision = 'b' * 40
        other = self.root / 'other'
        other.mkdir()
        self.root = other
        replacement = self.release('win-x64')
        with self.assertRaisesRegex(ValueError, 'same version and source revision'):
            assembly.assemble([self.releases[0], replacement, self.releases[2]], self.output)

    def test_signed_package_cannot_be_repacked(self):
        package = self.releases[0] / 'packages' / f'Isoplane.Cli.{self.version}.nupkg'
        with zipfile.ZipFile(package, 'a') as archive:
            archive.writestr('.signature.p7s', b'signature')
        with self.assertRaisesRegex(ValueError, 'before signing'):
            assembly.combine_package(package, self.root / 'output.nupkg', {})

    def test_duplicate_archive_entries_are_rejected(self):
        import warnings
        package = self.releases[0] / 'packages' / f'Isoplane.Cli.{self.version}.nupkg'
        with warnings.catch_warnings():
            warnings.simplefilter('ignore', UserWarning)
            with zipfile.ZipFile(package, 'a') as archive:
                archive.writestr('README.md', b'duplicate')
        with self.assertRaisesRegex(ValueError, 'duplicate ZIP'):
            assembly.combine_package(package, self.root / 'output.nupkg', {})
