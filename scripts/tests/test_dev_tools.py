"""Development installations are keyed by tool versions, not worktree locations."""

import importlib.util
import json
import os
import signal
from pathlib import Path
import sys
import subprocess
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parents[1]))
SPEC = importlib.util.spec_from_file_location('dev_tools', Path(__file__).parents[1] / 'dev_tools.py')
tools = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(tools)
ROOT = Path(__file__).resolve().parents[2]


@unittest.skipIf(os.name == 'nt', 'POSIX executable tool fixtures')
class NestedSetup(unittest.TestCase):
    def run_setup(self, damaged=False, nested=False, missing_workload=False):
        with tempfile.TemporaryDirectory() as temporary:
            cache = Path(temporary) / 'cache'
            name, wasm, rid = tools.identity(ROOT)
            sdk = json.loads((ROOT / 'global.json').read_text())['sdk']['version']
            with patch.object(tools.storage, 'tools_root', return_value=cache):
                with tools.storage.product(name, root=cache) as directory:
                    for path, value in [(directory / 'bin/wasm-tools', 'wasm-tools ' + wasm),
                                        (directory / 'dotnet/dotnet', sdk),
                                        (directory / 'bin/rustup', '')]:
                        path.parent.mkdir(parents=True, exist_ok=True)
                        path.write_text('#!' + sys.executable + '\nprint(' + repr(value) + ')\n')
                        path.chmod(0o755)
                    if damaged:
                        (directory / 'bin/wasm-tools').unlink()
                    workload = directory / 'dotnet/workload-pack'
                    workload.write_bytes(b'installed workload')
                    (directory / 'dotnet.json').write_text(json.dumps({
                        'version': sdk, 'runtimeIdentifier': rid,
                        'files': tools.preview_builder(ROOT).inventory(directory / 'dotnet')}))
                    if missing_workload:
                        workload.unlink()
                    (directory / 'complete').write_text(name)
            launcher = Path(temporary) / 'launch.py'
            launcher.write_text('''import sys, runpy
from pathlib import Path
root, cache, mode = Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3]
sys.path.insert(0, str(root / 'scripts'))
import dev_tools
dev_tools.storage.tools_root = lambda: cache
if mode == 'setup':
    dev_tools.setup(root)
else:
    child = 'dev' if mode == 'nested' else 'setup'
    sys.argv = [str(root / 'scripts/dev'), sys.executable, __file__, str(root), str(cache), child]
    runpy.run_path(str(root / 'scripts/dev'), run_name='__main__')
''')
            command = [sys.executable, str(launcher), str(ROOT), str(cache), 'nested' if nested else 'dev']
            with subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                  text=True, start_new_session=True) as process:
                try:
                    stdout, stderr = process.communicate(timeout=15)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.communicate()
                    self.fail('setup waited for a lease held by its own development session')
                return subprocess.CompletedProcess(command, process.returncode, stdout, stderr)

    def test_setup_reuses_tools_while_its_parent_development_session_holds_them(self):
        result = self.run_setup()
        self.assertEqual(result.returncode, 0, 'setup inside development must finish: ' + result.stderr)
        self.assertIn('already installed and verified', result.stdout, 'setup must verify and reuse the shared installation')

    def test_repair_inside_development_fails_without_waiting_for_its_parent(self):
        result = self.run_setup(damaged=True)
        self.assertNotEqual(result.returncode, 0, 'damaged tools must not be reported as installed')
        self.assertIn('Exit the development shell', result.stderr, 'repair must explain how to release the parent lease')

    def test_nested_development_sessions_can_run_setup(self):
        result = self.run_setup(nested=True)
        self.assertEqual(result.returncode, 0, 'nested development leases must permit setup to reuse tools: ' + result.stderr)

    def test_missing_workload_is_detected_even_when_tool_versions_are_correct(self):
        result = self.run_setup(missing_workload=True)
        self.assertNotEqual(result.returncode, 0, 'missing recorded workload bytes must invalidate the installation')
        self.assertIn('need repair', result.stderr, 'setup must diagnose workload damage while preserving the parent lease')


class SharedToolIdentity(unittest.TestCase):
    def test_identical_worktrees_share_tools_and_sdk_changes_select_another_installation(self):
        with tempfile.TemporaryDirectory() as temporary:
            roots = [Path(temporary) / name for name in ('first', 'second')]
            for root in roots:
                root.mkdir()
                (root / 'global.json').write_text('{"sdk":{"version":"10.0.100"}}')
                (root / 'Dockerfile').write_text('RUN cargo install --locked wasm-tools@1.244.0\n')
            self.assertEqual(tools.identity(roots[0]), tools.identity(roots[1]),
                             'worktree paths must not multiply identical tool installations')
            (roots[1] / 'global.json').write_text('{"sdk":{"version":"10.0.101"}}')
            self.assertNotEqual(tools.identity(roots[0]), tools.identity(roots[1]),
                                'different SDK versions must have independent installations')
