from importlib.machinery import SourceFileLoader
import importlib.util
import os
from pathlib import Path
import tempfile
import subprocess
import sys
import unittest

spec = importlib.util.spec_from_loader('dev', SourceFileLoader('dev', str(Path(__file__).parents[1] / 'dev')))
dev = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dev)


class DevelopmentEnvironmentTests(unittest.TestCase):
    def test_finishing_a_development_command_reclaims_its_oversized_intermediates(self):
        with tempfile.TemporaryDirectory() as temporary:
            shared = Path(temporary)
            launcher = """import sys,runpy
from pathlib import Path
sys.path.insert(0, str(Path(sys.argv[2]).parent))
import dev_tools
shared_path = Path(sys.argv[1])
dev_tools.storage.shared_root = lambda: shared_path
dev_tools.storage.PRODUCT_BYTES = 0
sys.argv = sys.argv[2:]
runpy.run_path(sys.argv[0], run_name='__main__')
"""
            command = "import os; from pathlib import Path; p=Path(os.environ['CARGO_TARGET_DIR']); p.mkdir(parents=True); (p/'artifact').write_bytes(b'compiled'); print(p)"
            environment = {key: value for key, value in os.environ.items()
                           if key not in ('CARGO_TARGET_DIR', 'CARGO_BUILD_BUILD_DIR', 'ISOPLANE_BUILD_ROOT', 'ISOPLANE_BUILD_DIRECTORY')}
            result = subprocess.run([sys.executable, '-c', launcher, str(shared), str(Path(dev.__file__)),
                                     sys.executable, '-c', command], env=environment,
                                    capture_output=True, text=True, timeout=30)
            self.assertEqual(result.returncode, 0, 'the development command must complete: ' + result.stderr)
            target = Path(result.stdout.strip())
            self.assertTrue(target.is_relative_to(shared), 'default Cargo output must belong to managed storage')
            self.assertFalse(target.exists(), 'command completion must trigger retention without a manual cleaner')

    def test_repository_tools_are_selected_without_changing_the_parent_environment(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            tools = root / 'shared-tools'
            sdk = tools / 'dotnet'
            sdk.mkdir(parents=True)
            (sdk / 'dotnet').touch()
            wasi = root / 'guest-runtime/work/wasi-sdk'
            (wasi / 'bin').mkdir(parents=True)
            (wasi / 'bin/clang').touch()
            inherited = {'PATH': '/system', 'DOTNET_ROOT': '/system/dotnet'}
            configured = dev.development_environment(root, inherited, tools)
            self.assertEqual(configured['PATH'].split(os.pathsep)[:2],
                             [str(tools / 'bin'), str(sdk)],
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
