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
        environment = patch.dict(packages.os.environ)
        environment.start()
        self.addCleanup(environment.stop)
        packages.os.environ.pop('ISOPLANE_TEST_RUNTIME_MODULES', None)
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        for name in ("guest-sdks/dotnet", "tools", "crates", ".cargo", "scripts"):
            (self.root / name).mkdir(parents=True, exist_ok=True)
        for name in ("Cargo.toml", "Cargo.lock", "Directory.Build.props", "global.json",
                     "nuget.config", "scripts/fixture-tools.py", "scripts/fixture-packages.py"):
            (self.root / name).write_text("input")
        toolchain = patch.object(packages.subprocess, "check_output", return_value=b"toolchain")
        toolchain.start()
        self.addCleanup(toolchain.stop)
        builder = patch.object(packages.subprocess, "run", side_effect=self.build)
        self.builder = builder.start()
        self.addCleanup(builder.stop)

    def build(self, command, **kwargs):
        if "--fixture" in command:
            runtime = self.root / "guest-runtime/tests/packages/out/runtime"
            runtime.mkdir(parents=True, exist_ok=True)
            for path in packages.modules.files(runtime):
                if not path.exists():
                    path.write_bytes(b"runtime")
            return subprocess.CompletedProcess(command, 0)
        output = Path(command[command.index("-o") + 1])
        output.mkdir(parents=True, exist_ok=True)
        (output / (Path(command[2]).stem + ".nupkg")).write_bytes(b"package")
        return subprocess.CompletedProcess(command, 0)

    def test_unchanged_consumers_reuse_packages_and_corruption_rebuilds_them(self):
        def packs():
            return sum(1 for call in self.builder.call_args_list if "pack" in call.args[0])
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
        original = packages.package_feed(self.root)
        (self.root / "tools" / "Changed.cs").write_text("updated CLI")
        self.assertNotEqual(packages.package_feed(self.root), original,
                            "changed source must not reuse packages with the same version")

    def test_a_rebuilt_shared_runtime_selects_a_new_package_feed(self):
        original = packages.package_feed(self.root)
        (self.root / "guest-runtime/tests/packages/out/runtime/Isoplane.MonoRuntime.wasm").write_bytes(b"rebuilt")
        self.assertNotEqual(packages.package_feed(self.root), original,
                            "an SDK package must not keep a runtime its source no longer builds")

    def test_changed_runtime_provenance_selects_a_new_package_feed(self):
        original = packages.package_feed(self.root)
        (self.root / "guest-runtime/tests/packages/out/runtime/runtime-build.json").write_text("new provenance")
        self.assertNotEqual(packages.package_feed(self.root), original,
                            "an SDK package must not keep superseded runtime provenance")


if __name__ == "__main__":
    unittest.main()
