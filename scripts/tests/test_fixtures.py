"""Check fixture portability through the helper's command-line interface."""

from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
TOOLS = ROOT / "scripts/fixture-tools.py"


class FixtureTests(unittest.TestCase):
    def test_the_aspnet_builder_does_not_reacquire_its_parents_fixture_lock(self):
        import os
        with tempfile.TemporaryDirectory() as directory:
            interpreter = Path(directory) / "python3"
            interpreter.write_text('#!/bin/sh\ncase "$1" in */aspnet/build.py) exit 0;; *) exit 23;; esac\n')
            interpreter.chmod(0o755)
            environment = dict(os.environ, PATH=directory + os.pathsep + os.environ["PATH"],
                               ISOPLANE_FIXTURE_LOCK=str(ROOT / "guests/build-dotnet.sh"))
            result = subprocess.run(["bash", str(ROOT / "guests/aspnet/build.sh")], env=environment)
            self.assertEqual(result.returncode, 0, "a nested guest build must not wait on the lock held by its parent")

    def test_an_unknown_dotnet_fixture_is_rejected_before_building(self):
        result = subprocess.run(["bash", str(ROOT / "guests/build-dotnet.sh"),
                                 "--if-stale", "--fixture", "not-a-fixture"],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 2, "an unknown fixture must fail before invoking the toolchain")
        self.assertIn("unknown C# fixture for runtime nativeaot: not-a-fixture", result.stderr,
                      "the fixture error must identify the requested name")

    def test_selecting_one_dotnet_fixture_builds_only_that_fixture(self):
        import os
        import shutil
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            guests = root / "guests"
            project = guests / "dotnet"
            project.mkdir(parents=True)
            scripts = root / "scripts"
            scripts.mkdir()
            shutil.copy2(ROOT / "guests/build-dotnet.sh", guests / "build-dotnet.sh")
            shutil.copy2(TOOLS, scripts / TOOLS.name)
            (guests / "aspnet").mkdir()
            (root / "guest-sdks/dotnet").mkdir(parents=True)
            (root / "nuget.config").write_text("<configuration />")
            (root / "global.json").write_text("{}")
            runtime = root / "guest-runtime"
            feed = runtime / "out/feed"
            feed.mkdir(parents=True)
            (feed / "Isoplane.GuestRuntime.props").write_text(
                "<Project><IsoplaneGuestNativeAotPackVersion>test</IsoplaneGuestNativeAotPackVersion>"
                "<IsoplaneGuestNativeAotPackSha512>digest</IsoplaneGuestNativeAotPackSha512></Project>")
            # This test isolates variant selection; feed integrity has its own real verifier tests.
            (runtime / "feed-identity.py").write_text(
                "import sys\nfrom pathlib import Path\n"
                "assert sys.argv[1] == 'verify-current'\n"
                "Path(sys.argv[2], 'verified').write_text(sys.argv[3])\n")
            compiler = runtime / "compiler"
            compiler.mkdir()
            (compiler / "build.py").write_text(
                "import sys\nfrom pathlib import Path\n"
                "assert sys.argv[1:3] == ['verify-current', '--output']\n"
                "output = Path(sys.argv[3])\noutput.mkdir(parents=True)\n"
                "(output / 'compiler.json').write_text('{}')\n")
            builder = project / "build.sh"
            builder.write_text('#!/bin/sh\nmkdir -p "$ISOPLANE_OUT_DIR"\nprintf fixture > "$ISOPLANE_OUT_DIR/$ISOPLANE_OUT.wasm"\nprintf test@sha512:digest > "$ISOPLANE_OUT_DIR/$ISOPLANE_OUT.wasm.runtime"\n')
            builder.chmod(0o755)
            tools = root / "tools"
            tools.mkdir()
            dotnet = tools / "dotnet"
            dotnet.write_text("#!/bin/sh\nexit 0\n")
            dotnet.chmod(0o755)
            selected = "isoplane_guest_dotnet_connect"
            result = subprocess.run(["bash", str(guests / "build-dotnet.sh"), "--fixture", selected],
                                    env=dict(os.environ, PATH=str(tools) + os.pathsep + os.environ["PATH"]),
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(sorted(path.name for path in (project / "out").iterdir()),
                             sorted([selected + suffix for suffix in (".wasm", ".wasm.runtime", ".wasm.fixture.json")]),
                             "fixture selection must not build or reuse another variant")
            self.assertEqual(Path((runtime / "verified").read_text()).resolve(), feed.resolve(),
                             "default fixture selection must verify the current feed before building")
            self.assertTrue((runtime / "out/compiler/compiler.json").is_file(),
                            "NativeAOT fixture selection must verify its compiler inputs before building")

    def test_the_compiler_image_recipe_invalidates_cached_fixtures(self):
        with tempfile.TemporaryDirectory() as scratch:
            recipe = Path(scratch) / "Dockerfile"
            recipe.write_text("FROM compiler:pinned")
            before = subprocess.check_output(["python3", str(TOOLS), "fingerprint", scratch], text=True)
            recipe.write_text("FROM compiler:updated")
            after = subprocess.check_output(["python3", str(TOOLS), "fingerprint", scratch], text=True)
            self.assertNotEqual(before, after, "fixture freshness must include the compiler image recipe")

    def test_source_changes_invalidate_a_fixture_with_preserved_timestamps(self):
        import os
        with tempfile.TemporaryDirectory() as scratch:
            source = Path(scratch) / "guest.cs"
            source.write_text("first")
            stamp = source.stat()
            before = subprocess.check_output(["python3", str(TOOLS), "fingerprint", scratch], text=True)
            source.write_text("second")
            os.utime(source, ns=(stamp.st_atime_ns, stamp.st_mtime_ns))
            after = subprocess.check_output(["python3", str(TOOLS), "fingerprint", scratch], text=True)
            self.assertNotEqual(before, after, "editing source must invalidate its fixture despite preserved timestamps")

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
