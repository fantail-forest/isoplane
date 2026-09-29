"""Check fixture portability through the helper's command-line interface."""

from pathlib import Path
import subprocess
import os
import shutil
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
TOOLS = ROOT / "scripts/fixture-tools.py"


def stage_dotnet_fixture(root, build_script):
    guests = root / "guests"
    project = guests / "dotnet"
    project.mkdir(parents=True)
    scripts = root / "scripts"
    scripts.mkdir()
    shutil.copy2(ROOT / "guests/build-dotnet.sh", guests / "build-dotnet.sh")
    shutil.copy2(TOOLS, scripts / TOOLS.name)
    shutil.copy2(ROOT / "scripts/build_storage.py", scripts / "build_storage.py")
    (guests / "aspnet").mkdir()
    (root / "guest-sdks/dotnet").mkdir(parents=True)
    (root / "nuget.config").write_text("<configuration />")
    (root / "global.json").write_text("{}")
    runtime = root / "guest-runtime"
    feed = runtime / "out/feed"
    feed.mkdir(parents=True)
    (feed / "Isoplane.GuestRuntime.props").write_text(
        "<Project><IsoplaneGuestRuntimePackVersion>test</IsoplaneGuestRuntimePackVersion>"
        "<IsoplaneGuestRuntimePackSha512>digest</IsoplaneGuestRuntimePackSha512></Project>")
    # This test isolates variant selection; feed integrity has its own real verifier tests.
    (runtime / "feed-identity.py").write_text(
        "import sys\nfrom pathlib import Path\n"
        "assert sys.argv[1] == 'verify-current'\n"
        "Path(sys.argv[2], 'verified').write_text(sys.argv[3])\n")
    builder = project / "build.sh"
    builder.write_text(build_script)
    builder.chmod(0o755)
    tools = root / "tools"
    tools.mkdir()
    dotnet = tools / "dotnet"
    dotnet.write_text("#!/bin/sh\nexit 0\n")
    dotnet.chmod(0o755)


def build_dotnet_fixture(root):
    return subprocess.run(["bash", str(root / "guests/build-dotnet.sh"), "--fixture", "isoplane_guest_dotnet_connect"],
                          env=dict(os.environ, PATH=str(root / "tools") + os.pathsep + os.environ["PATH"]),
                          capture_output=True, text=True)


class FixtureTests(unittest.TestCase):

    def test_an_unknown_dotnet_fixture_is_rejected_before_building(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            (root / "guests").mkdir()
            (root / "scripts").mkdir()
            builder = root / "guests/build-dotnet.sh"
            shutil.copy2(ROOT / "guests/build-dotnet.sh", builder)
            shutil.copy2(TOOLS, root / "scripts/fixture-tools.py")
            result = subprocess.run(["bash", str(builder), "--if-stale", "--fixture", "not-a-fixture"],
                                    capture_output=True, text=True)
        self.assertEqual(result.returncode, 2, "an unknown fixture must fail before invoking the toolchain")
        self.assertIn("unknown C# fixture for runtime mono: not-a-fixture", result.stderr,
                      "the fixture error must identify the requested name")

    def test_selecting_one_dotnet_fixture_builds_only_that_fixture(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            stage_dotnet_fixture(root, '#!/bin/sh\nmkdir -p "$ISOPLANE_OUT_DIR"\nprintf fixture > "$ISOPLANE_OUT_DIR/$ISOPLANE_OUT.wasm"\nprintf test@sha512:digest > "$ISOPLANE_OUT_DIR/$ISOPLANE_OUT.wasm.runtime"\n')
            result = build_dotnet_fixture(root)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(sorted(path.name for path in (root / "guests/dotnet/out/mono").iterdir()),
                             sorted(["isoplane_guest_dotnet_connect" + suffix for suffix in (".wasm", ".wasm.runtime", ".wasm.fixture.json")]),
                             "fixture selection must not build or reuse another variant")
            runtime = root / "guest-runtime"
            self.assertEqual(Path((runtime / "verified").read_text()).resolve(), (runtime / "out/feed").resolve(),
                             "default fixture selection must verify the current feed before building")

    def test_a_rebuild_keeps_the_published_fixture_available_to_existing_readers(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            stage_dotnet_fixture(root, '#!/bin/sh\nset -eu\ntest "$(cat "$ISOPLANE_OUT_DIR/$ISOPLANE_OUT.wasm")" = published\nprintf replacement > "$ISOPLANE_OUT_DIR/$ISOPLANE_OUT.wasm"\nprintf test@sha512:digest > "$ISOPLANE_OUT_DIR/$ISOPLANE_OUT.wasm.runtime"\n')
            output = root / "guests/dotnet/out/mono/isoplane_guest_dotnet_connect.wasm"
            output.parent.mkdir(parents=True)
            output.write_text("published")
            result = build_dotnet_fixture(root)
            self.assertEqual(result.returncode, 0,
                             "a concurrent reader must retain the old publication until the builder replaces it: " + result.stdout + result.stderr)
            self.assertEqual(output.read_text(), "replacement", "the successful builder must publish the replacement")

    def test_a_package_rebuild_retains_the_published_archive_until_replacement(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            packages = root / "guest-runtime/tests/packages"
            packages.mkdir(parents=True)
            shutil.copy2(ROOT / "guest-runtime/tests/packages/build.sh", packages / "build.sh")
            output = packages / "out/alpha.tar"
            output.parent.mkdir()
            output.write_text("published")
            runtime = output.parent / "runtime"
            runtime.mkdir()
            (runtime / "Isoplane.MonoRuntime.wasm.runtime").write_text("identity")
            (root / "guest-runtime/feed-identity.py").write_text("")
            scripts = root / "scripts"
            scripts.mkdir()
            (scripts / "fixture-tools.py").write_text(
                "import sys\n"
                "if sys.argv[1] in ('fingerprint', 'runtime-identity', 'isoplane-cli'): print('identity')\n")
            tools = root / "tools"
            tools.mkdir()
            dotnet = tools / "dotnet"
            dotnet.write_text(
                "#!/usr/bin/env python3\nimport sys\nfrom pathlib import Path\n"
                "output = Path(sys.argv[sys.argv.index('--output') + 1])\n"
                "assert output.read_text() == 'published', 'the previous package vanished during its rebuild'\n"
                "output.write_text('replacement')\n")
            dotnet.chmod(0o755)
            environment = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ["PATH"])
            environment.pop("ISOPLANE_TEST_RUNTIME_FEED", None)
            result = subprocess.run(["bash", str(packages / "build.sh"), "--fixture", "alpha"],
                                    env=environment, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, "package rebuilds must preserve existing readers: " + result.stdout + result.stderr)
            self.assertEqual(output.read_text(), "replacement", "the successful bundler must replace the old package")

    def test_a_build_script_change_invalidates_cached_fixtures(self):
        with tempfile.TemporaryDirectory() as scratch:
            recipe = Path(scratch) / "build.sh"
            recipe.write_text("dotnet publish -c Debug")
            before = subprocess.check_output(["python3", str(TOOLS), "fingerprint", scratch], text=True)
            recipe.write_text("dotnet publish -c Release")
            after = subprocess.check_output(["python3", str(TOOLS), "fingerprint", scratch], text=True)
            self.assertNotEqual(before, after, "fixture freshness must include its build script")

    def test_source_changes_invalidate_a_fixture_with_preserved_timestamps(self):
        with tempfile.TemporaryDirectory() as scratch:
            source = Path(scratch) / "guest.cs"
            source.write_text("first")
            stamp = source.stat()
            before = subprocess.check_output(["python3", str(TOOLS), "fingerprint", scratch], text=True)
            source.write_text("second")
            os.utime(source, ns=(stamp.st_atime_ns, stamp.st_mtime_ns))
            after = subprocess.check_output(["python3", str(TOOLS), "fingerprint", scratch], text=True)
            self.assertNotEqual(before, after, "editing source must invalidate its fixture despite preserved timestamps")
