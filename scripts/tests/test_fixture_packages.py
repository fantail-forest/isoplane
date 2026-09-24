"""Package reuse requires unchanged inputs and intact output artifacts."""
import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("fixture_packages", Path(__file__).parents[1] / "fixture-packages.py")
packages = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(packages)


class PackageFeed(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        for name in ("guest-sdks/dotnet", "tools", "crates", ".cargo", "scripts"):
            (self.root / name).mkdir(parents=True, exist_ok=True)
        for name in ("Cargo.toml", "Cargo.lock", "Directory.Build.props", "global.json",
                     "nuget.config", "scripts/fixture-tools.py", "scripts/fixture-packages.py"):
            (self.root / name).write_text("input")

    def build(self, command, **kwargs):
        if "--fixture" in command:
            runtime = self.root / "guest-runtime/tests/packages/out/runtime"
            runtime.mkdir(parents=True, exist_ok=True)
            for name in ("Isoplane.MonoRuntime.wasm", "Isoplane.MonoRuntime.Debug.wasm", "runtime-build.json"):
                if not (runtime / name).exists():
                    (runtime / name).write_bytes(b"runtime")
            return subprocess.CompletedProcess(command, 0)
        output = Path(command[command.index("-o") + 1])
        output.mkdir(parents=True, exist_ok=True)
        (output / (Path(command[2]).stem + ".nupkg")).write_bytes(b"package")
        return subprocess.CompletedProcess(command, 0)

    def test_unchanged_consumers_reuse_packages_and_corruption_rebuilds_them(self):
        with patch.object(packages.subprocess, "check_output", return_value=b"toolchain"), \
                patch.object(packages.subprocess, "run", side_effect=self.build) as build:
            def packs():
                return sum(1 for call in build.call_args_list if "pack" in call.args[0])
            feed = packages.package_feed(self.root)
            initial_packs = packs()
            self.assertGreater(initial_packs, 0, "a new package feed must build its packages")
            self.assertEqual(packages.package_feed(self.root), feed,
                             "unchanged inputs must identify the same feed")
            self.assertEqual(packs(), initial_packs,
                             "a second consumer must not repack unchanged SDK and CLI artifacts")
            next(feed.glob("*.nupkg")).write_bytes(b"corrupt")
            packages.package_feed(self.root)
            self.assertGreater(packs(), initial_packs,
                               "a modified package must not be reused")

    def test_a_source_change_selects_a_new_package_feed(self):
        with patch.object(packages.subprocess, "check_output", return_value=b"toolchain"), \
                patch.object(packages.subprocess, "run", side_effect=self.build):
            original = packages.package_feed(self.root)
            (self.root / "tools" / "Changed.cs").write_text("updated CLI")
            self.assertNotEqual(packages.package_feed(self.root), original,
                                "changed source must not reuse packages with the same version")

    def test_a_rebuilt_shared_runtime_selects_a_new_package_feed(self):
        with patch.object(packages.subprocess, "check_output", return_value=b"toolchain"), \
                patch.object(packages.subprocess, "run", side_effect=self.build):
            original = packages.package_feed(self.root)
            (self.root / "guest-runtime/tests/packages/out/runtime/Isoplane.MonoRuntime.wasm").write_bytes(b"rebuilt")
            self.assertNotEqual(packages.package_feed(self.root), original,
                                "an SDK package must not keep a runtime its source no longer builds")


if __name__ == "__main__":
    unittest.main()
