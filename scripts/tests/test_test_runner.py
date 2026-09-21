"""The concurrent runner retains Cargo's test inventory and propagates failures."""
from importlib.machinery import SourceFileLoader
import importlib.util
import os
from pathlib import Path
import sys
import tempfile
import unittest

PATH = Path(__file__).parents[1] / "test"
SPEC = importlib.util.spec_from_loader("test_runner", SourceFileLoader("test_runner", str(PATH)))
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)


class TestRunner(unittest.TestCase):
    def test_preparation_deadlines_run_without_competing_binaries(self):
        binaries = ["isoplane-native/installed_dev", "isoplane-prepare/process", "isoplane-host/io"]
        batches = runner.execution_batches(binaries, lambda name: name)
        self.assertEqual(batches, [["isoplane-prepare/process"],
                                  ["isoplane-native/installed_dev", "isoplane-host/io"]],
                         "preparation deadlines need isolation without losing other tests")

    def test_only_test_executables_run_once(self):
        artifact = {"reason": "compiler-artifact", "profile": {"test": True},
                    "executable": "/build/deps/guest-tests"}
        binaries, paths = runner.test_binaries([
            artifact, artifact,
            {**artifact, "profile": {"test": False}},
            {**artifact, "executable": None},
            {"reason": "build-script-executed", "linked_paths": ["native=/build/native"]},
        ])
        self.assertEqual(binaries, [artifact], "duplicate Cargo messages must not rerun a suite")
        self.assertEqual(paths, {"/build/deps", "/build/native"},
                         "tests need both their dependencies and native build outputs")

    def test_a_failing_binary_retains_its_output_and_status(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result = runner.run([sys.executable, "-c", "print('failed assertion'); raise SystemExit(7)"],
                                root, os.environ.copy(), root / "output.log", 30)
            self.assertEqual(result["code"], 7, "test failure must reach the suite verdict")
            self.assertIn("failed assertion", (root / "output.log").read_text(),
                          "the failing test's diagnosis must remain available")

    def test_a_hung_binary_fails_instead_of_blocking_the_runner(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result = runner.run([sys.executable, "-c", "import time; time.sleep(300)"],
                                root, os.environ.copy(), root / "output.log", 0.1)
            self.assertEqual(result["code"], 124, "a hung binary must fail the suite")


if __name__ == "__main__":
    unittest.main()
