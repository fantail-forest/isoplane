"""Cleanup argument handling must not delete artifacts on help or invalid input."""
import pathlib
import shutil
import subprocess
import tempfile
import unittest


class CleanArguments(unittest.TestCase):
    def test_help_and_invalid_arguments_preserve_intermediates(self):
        for arguments, code in [(["--help"], 0), (["-h"], 0), (["--unknown"], 2), (["--all", "extra"], 2)]:
            with self.subTest(arguments=arguments), tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                (root / "scripts").mkdir()
                script = root / "scripts/clean"
                shutil.copyfile(pathlib.Path(__file__).resolve().parents[1] / "clean", script)
                artifact = root / "guests/target/keep"
                artifact.parent.mkdir(parents=True)
                artifact.write_text("cached")
                result = subprocess.run(["bash", str(script), *arguments], capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, code, "Argument handling must exit before cleanup with the appropriate status")
                self.assertTrue(artifact.exists(), "Help and invalid arguments must leave build artifacts untouched")


if __name__ == "__main__":
    unittest.main()
