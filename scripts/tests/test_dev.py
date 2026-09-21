from importlib.machinery import SourceFileLoader
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_loader('dev', SourceFileLoader('dev', str(Path(__file__).parents[1] / 'dev')))
dev = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dev)


class DevelopmentEnvironmentTests(unittest.TestCase):
    def test_repository_tools_are_selected_without_changing_the_parent_environment(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sdk = root / '.isoplane-dev/tools/dotnet'
            sdk.mkdir(parents=True)
            (sdk / 'dotnet').touch()
            wasi = root / 'guest-runtime/work/wasi-sdk'
            (wasi / 'bin').mkdir(parents=True)
            (wasi / 'bin/clang').touch()
            inherited = {'PATH': '/system', 'DOTNET_ROOT': '/system/dotnet'}
            configured = dev.development_environment(root, inherited)
            self.assertEqual(configured['PATH'].split(os.pathsep)[:2],
                             [str(root / '.isoplane-dev/tools/bin'), str(sdk)],
                             'repository commands must find the provisioned tools before system tools')
            self.assertEqual(configured['DOTNET_ROOT'], str(sdk), 'apphosts must use the same SDK installation')
            self.assertEqual(configured['WASI_SDK_PATH'], str(wasi), 'ABI fixtures must find the built WASI toolchain')
            self.assertEqual(inherited['DOTNET_ROOT'], '/system/dotnet', 'repository setup must not change the parent environment')

    def test_an_explicit_wasi_toolchain_is_preserved(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'guest-runtime/work/wasi-sdk/bin').mkdir(parents=True)
            (root / 'guest-runtime/work/wasi-sdk/bin/clang').touch()
            configured = dev.development_environment(root, {'PATH': '/system', 'WASI_SDK_PATH': '/chosen'})
            self.assertEqual(configured['WASI_SDK_PATH'], '/chosen', 'an explicit compiler selection takes precedence')
            self.assertNotIn('DOTNET_ROOT', configured, 'an absent repository SDK must not hide the system SDK')
