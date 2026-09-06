"""Check fixture portability through the helper's command-line interface."""

from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
TOOLS = ROOT / "scripts/fixture-tools.py"


class FixtureTests(unittest.TestCase):
    def test_the_compiler_image_recipe_invalidates_cached_fixtures(self):
        import os
        with tempfile.TemporaryDirectory() as scratch:
            recipe = Path(scratch) / "Dockerfile"
            recipe.write_text("FROM compiler:pinned")
            os.utime(recipe, ns=(123456789, 123456789))
            newest = subprocess.check_output(["python3", str(TOOLS), "newest", scratch], text=True)
            self.assertEqual(int(newest), 123456789, "fixture freshness must include the pinned compiler image recipe")

    def test_source_changes_invalidate_a_fixture_timestamp(self):
        with tempfile.TemporaryDirectory() as scratch:
            source = Path(scratch) / "guest.cs"
            source.write_text("first")
            before = subprocess.check_output(["python3", str(TOOLS), "newest", scratch], text=True)
            import os
            os.utime(source, ns=(int(before) + 1_000_000_000, int(before) + 1_000_000_000))
            after = subprocess.check_output(["python3", str(TOOLS), "newest", scratch], text=True)
            self.assertGreater(int(after), int(before), "editing a source did not invalidate its fixture")

    def test_the_build_lock_is_held_until_the_builder_exits(self):
        import fcntl
        with tempfile.TemporaryDirectory() as scratch:
            lock = Path(scratch) / "build.lock"
            script = Path(scratch) / "build.sh"
            script.write_text('''python3 - "$1" <<'PROBE'
import fcntl, sys
with open(sys.argv[1], 'a') as handle:
    try:
        fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        pass
    else:
        raise AssertionError('another builder could take the lock during the build')
PROBE
''')
            result = subprocess.run(["python3", str(TOOLS), "lock", str(lock), str(script), str(lock)], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            with lock.open("a") as handle:
                fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)

    def test_a_failed_builder_returns_its_exit_status(self):
        with tempfile.TemporaryDirectory() as scratch:
            script = Path(scratch) / "build.sh"
            script.write_text("exit 23\n")
            result = subprocess.run(["python3", str(TOOLS), "lock", str(Path(scratch) / "lock"), str(script)])
            self.assertEqual(result.returncode, 23, "fixture helper concealed a failed build")
