"""An explicit native library remains part of a no-build NuGet package."""

from pathlib import Path
import platform
import subprocess
import tempfile
import unittest
import zipfile

ROOT = Path(__file__).resolve().parents[2]


class NativePackage(unittest.TestCase):
    def test_no_build_packing_includes_the_selected_native_library(self):
        with tempfile.TemporaryDirectory(prefix='isoplane-native-pack-') as temporary:
            directory = Path(temporary)
            name = {'Windows': 'isoplane_native.dll', 'Darwin': 'libisoplane_native.dylib'}.get(
                platform.system(), 'libisoplane_native.so')
            native = directory / name
            native.write_bytes(b'owned native packaging fixture')
            output = directory / 'packages'
            command = ['dotnet', 'pack', str(ROOT / 'tools/Isoplane.Native/Isoplane.Native.csproj'),
                       '--artifacts-path', str(directory / 'artifacts'), '-o', str(output),
                       f'-p:IsoplaneNativePath={native}', '--disable-build-servers']
            for extra in ([], ['--no-build', '--no-restore']):
                if extra:
                    for stale in directory.rglob('*.nupkg'):
                        stale.unlink()
                    for stale in directory.rglob('*.nuspec'):
                        stale.unlink()
                result = subprocess.run(command + extra, capture_output=True, text=True, timeout=120)
                self.assertEqual(result.returncode, 0, 'packing must succeed: ' + result.stdout + result.stderr)
            with zipfile.ZipFile(next(output.glob('*.nupkg'))) as package:
                libraries = [path for path in package.namelist() if path.endswith('/native/' + name)]
                self.assertEqual(len(libraries), 1, 'no-build packaging must include exactly one native library')
                self.assertEqual(package.read(libraries[0]), native.read_bytes(),
                                 'the package must contain the explicitly selected native bytes')
