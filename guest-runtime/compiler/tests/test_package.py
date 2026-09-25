import importlib.util
import json
import os
from pathlib import Path
import tempfile
import sys
import unittest
import xml.etree.ElementTree as ET
from unittest.mock import patch
import zipfile

MODULE = Path(__file__).resolve().parents[1] / 'build.py'
spec = importlib.util.spec_from_file_location('compiler_builder', MODULE)
compiler = importlib.util.module_from_spec(spec)
spec.loader.exec_module(compiler)


class NativeCacheTests(unittest.TestCase):
    def test_default_compiler_work_is_locked_and_reclaimed_automatically(self):
        with tempfile.TemporaryDirectory() as temporary:
            storage = compiler.build_storage()
            observed = []
            def produce(args):
                observed.append(args.work)
                (args.work / 'intermediate').write_bytes(b'compiled')
                with (args.work / '.build.lock').open('a+b') as competing:
                    self.assertFalse(storage.lock(competing, blocking=False),
                                     'concurrent compiler builds must not modify the same intermediates')
                storage.prune(Path(temporary), 0)
                self.assertTrue(args.work.exists(), 'active compiler intermediates must survive cleanup')
                return {'version': 'test'}
            with patch.object(storage, 'shared_root', return_value=Path(temporary)), \
                    patch.object(storage, 'PRODUCT_BYTES', 0), \
                    patch.object(compiler, 'build', side_effect=produce), \
                    patch.object(sys, 'argv', ['compiler', 'build']), \
                    patch.dict(os.environ, {'ISOPLANE_RUNTIME_SOURCE': ''}):
                compiler.main()
            self.assertEqual(len(observed), 1, 'the compiler command must execute inside its storage lease')
            self.assertFalse(observed[0].exists(), 'finished compiler work must obey automatic retention')

    def test_macos_arm64_manifest_selects_source_built_host_tools(self):
        with patch.object(compiler.platform, 'system', return_value='Darwin'), \
                patch.object(compiler.platform, 'machine', return_value='arm64'):
            spec = compiler.manifest()
        self.assertEqual(spec['runtime_identifier'], 'osx-arm64', 'Apple Silicon must select native tools')
        self.assertEqual(spec['package'], 'runtime.osx-arm64.Microsoft.DotNet.ILCompiler.LLVM',
                         'Mac compiler packages must not replace Linux packages')

    def test_unsupported_hosts_fail_before_building_linux_packages(self):
        with patch.object(compiler.platform, 'system', return_value='Linux'), \
                patch.object(compiler.platform, 'machine', return_value='aarch64'):
            with self.assertRaisesRegex(ValueError, 'unsupported compiler build host'):
                compiler.manifest()

    def test_moved_native_build_directories_are_reconfigured(self):
        with tempfile.TemporaryDirectory() as temporary:
            tree = Path(temporary)
            directory = tree / 'artifacts/obj/coreclr/linux.x64.Release'
            directory.mkdir(parents=True)
            cache = directory / 'CMakeCache.txt'
            cache.write_text(f'CMAKE_HOME_DIRECTORY:INTERNAL={tree / "src/coreclr"}\n'
                             f'CMAKE_CACHEFILE_DIR:INTERNAL={tree / "old-directory"}\n')
            source = tree / 'source-input'
            source.write_text('preserve')
            compiler.prepare_native_cache(tree, 'linux')
            self.assertFalse(directory.exists(), 'relocated native intermediates must be rebuilt')
            self.assertEqual(source.read_text(), 'preserve', 'cache repair must preserve source inputs')

    def test_native_builds_in_the_same_location_remain_incremental(self):
        with tempfile.TemporaryDirectory() as temporary:
            tree = Path(temporary)
            directory = tree / 'artifacts/obj/coreclr/windows.x64.Release'
            directory.mkdir(parents=True)
            cache = directory / 'CMakeCache.txt'
            cache.write_text(f'CMAKE_HOME_DIRECTORY:INTERNAL={tree / "src/coreclr"}\n'
                             f'CMAKE_CACHEFILE_DIR:INTERNAL={directory}\n')
            compiler.prepare_native_cache(tree, 'windows')
            self.assertTrue(cache.exists(), 'a cache at its original location must remain reusable')


class CompilerPackageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='isoplane-compiler-package-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.upstream = self.root / 'upstream.nupkg'
        with zipfile.ZipFile(self.upstream, 'w') as archive:
            archive.writestr('compiler.nuspec', '<package xmlns="urn:nuget"><metadata><id>runtime.linux-x64.Microsoft.DotNet.ILCompiler.LLVM</id><version>1.0</version><repository commit="source" /></metadata></package>')
            archive.writestr(compiler.BACKEND, b'upstream backend')
            archive.writestr('tools/ilc', b'pinned compiler executable')
            archive.writestr('LICENSE.TXT', b'original license')
        self.backend = self.root / 'backend.so'
        self.backend.write_bytes(b'corrected backend')
        self.managed = self.root / 'managed'
        self.managed.mkdir()
        (self.managed / 'ilc').write_bytes(b'source-built compiler')
        (self.managed / Path(compiler.BACKEND).name).write_bytes(self.backend.read_bytes())
        self.manifest = {'package': 'runtime.linux-x64.Microsoft.DotNet.ILCompiler.LLVM',
                         'sha256': compiler.digest(self.upstream), 'RUNTIMELAB_COMMIT': 'source',
                         'runtime_identifier': 'linux-x64'}
        self.mock = patch.object(compiler, 'manifest', return_value=self.manifest)
        self.mock.start()
        self.addCleanup(self.mock.stop)

    def compose(self):
        output = self.root / 'compiler'
        receipt = compiler.compose(self.upstream, self.backend, self.managed, output)
        return output, receipt

    def test_package_ships_source_built_compiler_and_preserves_licenses(self):
        output, receipt = self.compose()
        with zipfile.ZipFile(output / receipt['package']) as archive:
            self.assertEqual(archive.read('tools/ilc'), b'source-built compiler', 'the compiler executable must contain the source-built WASI policy')
            self.assertEqual(archive.read('LICENSE.TXT'), b'original license', 'upstream license notices must survive composition')
            self.assertEqual(archive.read(compiler.BACKEND), self.backend.read_bytes(), 'the composed package must use the corrected backend')
            self.assertIn(receipt['version'].encode(), archive.read('compiler.nuspec'), 'NuGet must resolve the distinct version')

    def test_windows_package_uses_the_windows_compiler_and_backend(self):
        self.manifest.update(package='runtime.win-x64.Microsoft.DotNet.ILCompiler.LLVM', runtime_identifier='win-x64')
        backend = compiler.backend_name('win-x64')
        (self.managed / 'ilc').rename(self.managed / 'ilc.exe')
        (self.managed / Path(compiler.BACKEND).name).rename(self.managed / Path(backend).name)
        output, receipt = self.compose()
        with zipfile.ZipFile(output / receipt['package']) as archive:
            self.assertEqual(archive.read('tools/ilc.exe'), b'source-built compiler', 'Windows must use the patched executable')
            self.assertEqual(archive.read(backend), self.backend.read_bytes(), 'Windows must use the patched DLL')
        self.assertIn(b'win-x64', (output / 'Isoplane.GuestCompiler.props').read_bytes(), 'the SDK must select the Windows compiler package')

    def test_macos_package_contains_only_source_built_host_tools(self):
        self.manifest.update(package='runtime.osx-arm64.Microsoft.DotNet.ILCompiler.LLVM',
                             runtime_identifier='osx-arm64', metadata_package=self.manifest['package'])
        with zipfile.ZipFile(self.upstream, 'a') as archive:
            archive.writestr('tools/linux-only.so', b'foreign binary')
            archive.writestr('[Content_Types].xml', '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="so" ContentType="application/octet-stream" /></Types>')
        self.manifest['sha256'] = compiler.digest(self.upstream)
        backend = compiler.backend_name('osx-arm64')
        (self.managed / Path(compiler.BACKEND).name).rename(self.managed / Path(backend).name)
        output, receipt = self.compose()
        with zipfile.ZipFile(output / receipt['package']) as archive:
            tools = {name for name in archive.namelist() if name.startswith('tools/')}
            self.assertEqual(tools, {'tools/ilc', backend}, 'foreign tools must never leak from the metadata package')
            self.assertEqual(archive.read(backend), self.backend.read_bytes(), 'Mac must ship its native backend')
            metadata = ET.fromstring(archive.read('compiler.nuspec'))
            self.assertEqual(metadata.findtext('./{*}metadata/{*}id'), self.manifest['package'],
                             'NuGet identity must select the Mac package')
            types = ET.fromstring(archive.read('[Content_Types].xml'))
            self.assertIn('dylib', {item.get('Extension') for item in types}, 'NuGet must recognize Mac libraries')
            self.assertTrue(archive.getinfo('tools/ilc').external_attr >> 16 & 0o111,
                            'the Mac compiler must remain executable after extraction')

    def test_changed_managed_compiler_bytes_get_a_distinct_version(self):
        _, first = self.compose()
        (self.managed / 'ilc').write_bytes(b'different compiled output')
        second = compiler.compose(self.upstream, self.backend, self.managed, self.root / 'changed')
        self.assertNotEqual(first['version'], second['version'], 'different managed compiler bytes must never reuse a NuGet version')

    def test_a_managed_publish_with_a_different_backend_is_refused(self):
        (self.managed / Path(compiler.BACKEND).name).write_bytes(b'wrong backend')
        with self.assertRaisesRegex(ValueError, 'different native backend'):
            self.compose()

    def test_corrupt_package_is_refused(self):
        output, receipt = self.compose()
        with (output / receipt['package']).open('ab') as stream:
            stream.write(b'corruption')
        with self.assertRaisesRegex(ValueError, 'does not match its receipt'):
            compiler.verify(output, False)

    def test_changed_msbuild_identity_is_refused(self):
        output, _ = self.compose()
        (output / 'Isoplane.GuestCompiler.props').write_text('<Project/>')
        with self.assertRaisesRegex(ValueError, 'properties do not match'):
            compiler.verify(output, False)

    def test_default_verification_refuses_stale_sources_but_explicit_artifact_verifies(self):
        output, _ = self.compose()
        with patch.object(compiler, 'identity', return_value='different source'):
            with self.assertRaisesRegex(ValueError, 'compiler is stale'):
                compiler.verify(output, True)
            compiler.verify(output, False)

    def test_upstream_package_from_different_inputs_is_refused(self):
        self.upstream.write_bytes(b'changed package')
        with self.assertRaisesRegex(ValueError, 'upstream compiler package hash mismatch'):
            self.compose()

    def test_tampered_receipt_cannot_substitute_the_nuget_hash(self):
        output, receipt = self.compose()
        receipt['package_sha512'] = 'invented hash'
        (output / 'compiler.json').write_text(json.dumps(receipt))
        with self.assertRaisesRegex(ValueError, 'SHA512 does not match'):
            compiler.verify(output, False)

    def test_recomposition_preserves_package_bytes_for_the_same_version(self):
        output, receipt = self.compose()
        second = self.root / 'second'
        other = compiler.compose(self.upstream, self.backend, self.managed, second)
        self.assertEqual(receipt['version'], other['version'], 'identical compiler inputs must keep their package version')
        self.assertEqual((output / receipt['package']).read_bytes(), (second / other['package']).read_bytes(), 'a package version must always identify the same bytes')

    def test_dependency_probe_cannot_differ_from_the_compiler_backend(self):
        output, _ = self.compose()
        (output / 'backend').write_bytes(b'other backend')
        with self.assertRaisesRegex(ValueError, 'probe backend does not match'):
            compiler.verify(output, False)

    def test_receipt_cannot_read_a_package_outside_its_directory(self):
        output, receipt = self.compose()
        receipt['package'] = '../upstream.nupkg'
        (output / 'compiler.json').write_text(json.dumps(receipt))
        with self.assertRaisesRegex(ValueError, 'package does not match'):
            compiler.verify(output, False)

    def test_existing_version_is_never_overwritten(self):
        self.compose()
        with self.assertRaisesRegex(ValueError, 'output already exists'):
            self.compose()


if __name__ == '__main__':
    unittest.main()
