"""Check native cleanup without touching the checkout's build outputs."""

from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class CleanupTests(unittest.TestCase):
    def test_cleanup_preserves_compiled_output(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            (root / "scripts").mkdir()
            script = root / "scripts/clean"
            script.write_text((ROOT / "scripts/clean").read_text())
            intermediate = root / "target/debug/incremental/cache"
            intermediate.parent.mkdir(parents=True)
            intermediate.write_text("cache")
            binary = root / "target/debug/wispd"
            binary.write_text("keep")
            result = subprocess.run(["bash", str(script)], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(intermediate.exists(), "cleanup left build intermediates")
            self.assertEqual(binary.read_text(), "keep", "cleanup removed the compiled daemon")
