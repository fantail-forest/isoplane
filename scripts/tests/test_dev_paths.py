"""Check launcher-owned scratch paths without replacing its process lifecycle."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class LauncherScratchTests(unittest.TestCase):
    def test_deep_worktrees_get_private_concurrent_scratch_and_clean_it_up(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / ("deep-worktree-" * 9)
            scripts = root / "scripts"
            scripts.mkdir(parents=True)
            shutil.copy2(ROOT / "scripts/dev", scripts / "dev")
            runtime = scripts / "node_modules/@anthropic-ai/sandbox-runtime/dist/cli.js"
            runtime.parent.mkdir(parents=True)
            runtime.touch()
            tools = Path(temporary) / "tools"
            tools.mkdir()
            node = tools / "node"
            node.write_text("""#!/usr/bin/python3
import json, os, pathlib, socket, sys
settings = json.loads(pathlib.Path(sys.argv[sys.argv.index('--settings') + 1]).read_text())
scratch = pathlib.Path(os.environ['TMPDIR'])
assert os.environ['CLAUDE_CODE_TMPDIR'] == str(scratch), 'runtime scratch paths disagree'
assert any(scratch.is_relative_to(path) for path in settings['filesystem']['allowWrite']), 'scratch is not writable inside sandbox'
assert '/home' in settings['filesystem']['denyRead'], 'home isolation was removed'
with socket.socket(socket.AF_UNIX) as listener:
    listener.bind(str(scratch / ('srt-mux-' + str(os.getpid()) + '-0.sock')))
    print(json.dumps({'scratch': str(scratch), 'mode': scratch.stat().st_mode & 0o777}), flush=True)
    sys.stdin.readline()
raise SystemExit(37)
""")
            node.chmod(0o755)
            environment = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ['PATH'])
            processes = []
            records = []
            try:
                for _ in range(2):
                    process = subprocess.Popen([str(scripts / 'dev'), 'true'], env=environment,
                                               stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                               stderr=subprocess.PIPE, text=True)
                    processes.append(process)
                    line = process.stdout.readline()
                    if not line:
                        _, error = process.communicate(timeout=20)
                        self.fail('deep-worktree launcher could not bind its runtime socket: ' + error)
                    records.append(json.loads(line))
                self.assertNotEqual(records[0]['scratch'], records[1]['scratch'],
                                    'concurrent launches shared runtime scratch')
                for record in records:
                    self.assertEqual(record['mode'], 0o700, 'runtime scratch is accessible to other users')
                for process in processes:
                    _, error = process.communicate('\n', timeout=20)
                    self.assertEqual(process.returncode, 37, 'launcher lost child failure: ' + error)
                for record in records:
                    self.assertFalse(Path(record['scratch']).exists(), 'launcher retained temporary sockets after child failure')
            finally:
                for process in processes:
                    if process.poll() is None:
                        process.communicate('\n', timeout=20)


if __name__ == '__main__':
    unittest.main()
