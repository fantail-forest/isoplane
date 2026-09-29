"""Package reuse requires unchanged inputs and intact output artifacts."""
import importlib.util
from contextlib import nullcontext
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
        self.destination = self.root / "consumer/packages"
        target = patch.object(packages.storage, "workspace",
                              side_effect=lambda name: nullcontext(self.root / "target/fixture-packages"))
        target.start()
        self.addCleanup(target.stop)
        for name in ("guest-sdks/dotnet", "tools", "crates", ".cargo", "scripts"):
            (self.root / name).mkdir(parents=True, exist_ok=True)
        for name in ("Cargo.toml", "Cargo.lock", "Directory.Build.props", "global.json",
                     "nuget.config", "scripts/fixture-tools.py", "scripts/fixture-packages.py"):
            (self.root / name).write_text("input")
        (self.root / 'Directory.Build.props').write_text(
            '<Project><PropertyGroup><Version>1.0.0</Version></PropertyGroup></Project>')
        shared = patch.object(packages.storage, 'shared_root', return_value=self.root / 'shared')
        shared.start()
        self.addCleanup(shared.stop)
        producer = patch.object(packages.storage.platform, 'system', return_value='Linux')
        producer.start()
        self.addCleanup(producer.stop)
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
        (output / (Path(command[2]).stem + ".1.0.0.nupkg")).write_bytes(b"package")
        return subprocess.CompletedProcess(command, 0)

    def test_unchanged_consumers_reuse_packages_and_corruption_rebuilds_them(self):
        def packs():
            return sum(1 for call in self.builder.call_args_list if "pack" in call.args[0])
        feed = packages.package_feed(self.root, self.destination)
        initial_packs = packs()
        self.assertGreater(initial_packs, 0, "a new package feed must build its packages")
        self.assertEqual(packages.package_feed(self.root, self.destination), feed,
                         "unchanged inputs must identify the same feed")
        self.assertEqual(packs(), initial_packs,
                         "a second consumer must not repack unchanged SDK and CLI artifacts")
        next((self.root / "target/fixture-packages/packages").glob("*.nupkg")).write_bytes(b"corrupt")
        packages.package_feed(self.root, self.destination)
        self.assertGreater(packs(), initial_packs,
                           "a modified package must not be reused")

    def assert_rebuild_reuses_intermediates(self, change):
        packages.package_feed(self.root, self.destination)
        before = self.builder.call_count
        change()
        packages.package_feed(self.root, self.destination)
        self.assertGreater(self.builder.call_count, before + 1,
                           "changed package inputs must rebuild the feed")
        artifacts = {call.args[0][call.args[0].index("--artifacts-path") + 1]
                     for call in self.builder.call_args_list if "pack" in call.args[0]}
        self.assertEqual(len(artifacts), 1,
                         "new source generations must reuse one incremental build directory")

    def test_changed_source_rebuilds_without_multiplying_build_trees(self):
        self.assert_rebuild_reuses_intermediates(lambda: (self.root / "tools/Changed.cs").write_text("updated"))

    def test_changed_content_discards_timestamp_based_managed_intermediates(self):
        packages.package_feed(self.root, self.destination)
        old = self.root / "target/fixture-packages/artifacts/obj/stale"
        old.parent.mkdir(parents=True)
        old.write_bytes(b"previous source generation")
        (self.root / "tools/Changed.cs").write_text("changed source")
        packages.package_feed(self.root, self.destination)
        self.assertFalse(old.exists(), "MSBuild must not reuse another generation's timestamp-based intermediate state")

    def test_a_rebuilt_runtime_rebuilds_the_packages(self):
        self.assert_rebuild_reuses_intermediates(lambda: (self.root / "guest-runtime/tests/packages/out/runtime/Isoplane.MonoRuntime.wasm").write_bytes(b"rebuilt"))

    def test_changed_runtime_provenance_rebuilds_the_packages(self):
        self.assert_rebuild_reuses_intermediates(lambda: (self.root / "guest-runtime/tests/packages/out/runtime/runtime-build.json").write_text("new provenance"))

    def test_the_consumer_receives_an_independent_copy(self):
        feed = packages.package_feed(self.root, self.destination)
        saved = {path.name: path.read_bytes() for path in feed.glob("*.nupkg")}
        for path in (self.root / "target/fixture-packages/packages").glob("*.nupkg"):
            path.unlink()
        self.assertEqual({path.name: path.read_bytes() for path in feed.glob("*.nupkg")}, saved,
                         "rebuilding the shared feed must not invalidate an active consumer")


if __name__ == "__main__":
    unittest.main()
