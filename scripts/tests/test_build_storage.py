"""Retention must bound owned output without deleting another run's files."""

import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import shutil
import tarfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("build_storage", Path(__file__).parents[1] / "build_storage.py")
storage = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(storage)


class Retention(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)

    def run_output(self, touched, size=4):
        with storage.retained_run(self.root, max_bytes=100, now=touched) as (directory, _):
            (directory / "build.log").write_bytes(b"x" * size)
        return directory

    def test_the_next_run_reclaims_expired_logs_automatically(self):
        old = self.run_output(1)
        with storage.retained_run(self.root, max_age_seconds=10, now=12):
            self.assertFalse(old.exists(), "starting a run must reclaim expired diagnostics")

    def test_finishing_a_run_evicts_the_oldest_logs_to_meet_the_budget(self):
        old = self.run_output(1)
        with storage.retained_run(self.root, max_bytes=5, now=2) as (current, _):
            (current / "build.log").write_bytes(b"1234")
        self.assertFalse(old.exists(), "the previous run must be evicted when the new output exceeds the budget")
        self.assertTrue(current.exists(), "the just-finished run must remain available for diagnosis")

    def test_retention_protects_a_run_held_by_another_process(self):
        with storage.retained_run(self.root, now=1) as (active, _):
            (active / "output").write_bytes(b"active")
            program = "import importlib.util,sys; s=importlib.util.spec_from_file_location('storage',sys.argv[1]); m=importlib.util.module_from_spec(s); s.loader.exec_module(m); m.prune(sys.argv[2],0,0,now=100)"
            subprocess.run([sys.executable, "-c", program, str(Path(storage.__file__)), str(self.root)], check=True)
            self.assertTrue(active.exists(), "a concurrent cleaner must preserve another process's live output")

    def test_unknown_directories_and_symlink_targets_are_never_deleted(self):
        unknown = self.root / "customer"
        unknown.mkdir()
        (unknown / "data").write_text("keep")
        link = self.root / "link"
        link.symlink_to(unknown, target_is_directory=True)
        storage.prune(self.root, 0, 0, now=100)
        self.assertTrue((unknown / "data").exists(), "retention must not adopt unowned directories")
        self.assertTrue(link.is_symlink(), "retention must not traverse or remove unowned links")

    def test_released_abandoned_output_is_reclaimed(self):
        program = "import importlib.util,sys,os; s=importlib.util.spec_from_file_location('storage',sys.argv[1]); m=importlib.util.module_from_spec(s); s.loader.exec_module(m); c=m.retained_run(sys.argv[2],now=1); d,_=c.__enter__(); (d/'output').write_bytes(b'abandoned'); os._exit(0)"
        subprocess.run([sys.executable, "-c", program, str(Path(storage.__file__)), str(self.root)], check=True)
        storage.prune(self.root, 0)
        self.assertEqual(list(self.root.glob("run-*")), [], "an exited owner must not leave output permanently exempt from retention")

    def test_worktrees_reuse_verified_downloads_without_another_fetch(self):
        source = self.root / "upstream"
        source.write_bytes(b"pinned toolchain")
        expected = storage.digest(source)
        with patch.object(storage, "shared_root", return_value=self.root / "shared"):
            first = self.root / "worktree-a/archive"
            storage.download(source.as_uri(), first, expected)
            source.unlink()
            second = self.root / "worktree-b/archive"
            storage.download(source.as_uri(), second, expected)
        self.assertEqual(first.read_bytes(), second.read_bytes(),
                         "another worktree must reuse verified shared bytes without reaching the origin")

    def test_an_invalid_download_preserves_the_consumers_previous_file(self):
        source = self.root / "upstream"
        source.write_bytes(b"wrong")
        destination = self.root / "output"
        destination.write_bytes(b"previous")
        with patch.object(storage, "shared_root", return_value=self.root / "shared"):
            with self.assertRaises(ValueError, msg="unverified bytes must never be published"):
                storage.download(source.as_uri(), destination, "0" * 64)
        self.assertEqual(destination.read_bytes(), b"previous",
                         "failed acquisition must leave the prior consumer artifact intact")

    def test_releasing_a_product_enforces_the_shared_budget(self):
        with patch.object(storage, "PRODUCT_BYTES", 1):
            with storage.product("large-product", root=self.root) as entry:
                (entry / "artifact").write_bytes(b"large")
                storage.prune(self.root, 1)
                self.assertTrue(entry.exists(), "shared product acquisition must protect live copies")
            self.assertFalse(entry.exists(), "a released oversized product must not escape automatic retention")

    def test_successful_scratch_is_removed_automatically(self):
        with patch.object(storage, "diagnostics_root", return_value=self.root):
            with storage.records() as directory:
                (directory / "output").write_bytes(b"scratch")
            self.assertFalse(directory.exists(), "successful scratch must not accumulate between runs")

    def test_failed_scratch_is_retained_for_diagnosis(self):
        with patch.object(storage, "diagnostics_root", return_value=self.root):
            with self.assertRaises(RuntimeError):
                with storage.records() as directory:
                    (directory / "output").write_bytes(b"failure")
                    raise RuntimeError("failed run")
            self.assertEqual((directory / "output").read_bytes(), b"failure",
                             "failed scratch must survive for diagnosis under automatic retention")

    def test_a_finished_oversized_run_does_not_escape_the_budget(self):
        with storage.retained_run(self.root, max_bytes=1) as (directory, _):
            (directory / "output").write_bytes(b"large")
        self.assertFalse(directory.exists(), "even the newest inactive run must respect the disk budget")

    def test_worktrees_reuse_immutable_products_and_verify_cached_bytes(self):
        produced = []
        first = self.root / "first/runtime.wasm"
        def build():
            first.parent.mkdir(exist_ok=True)
            first.write_bytes(b"compiled")
            produced.append(True)
        with patch.object(storage, "shared_root", return_value=self.root / "shared"):
            storage.cached_files("runtime", [first], build)
            second = self.root / "second/runtime.wasm"
            storage.cached_files("runtime", [second], lambda: self.fail("another worktree must reuse the verified product"))
            self.assertEqual(second.read_bytes(), b"compiled", "the consumer must receive the shared runtime bytes")
            second.write_bytes(b"consumer mutation")
            self.assertEqual(first.read_bytes(), b"compiled", "consumer modifications must not corrupt another worktree")
            cached = next((self.root / "shared").glob("*/0"))
            cached.write_bytes(b"corrupt")
            storage.cached_files("runtime", [first], build)
            self.assertEqual(len(produced), 2, "corrupt shared products must be rebuilt before reuse")

    @unittest.skipIf(sys.platform == "win32", "POSIX file descriptor inheritance")
    def test_a_child_build_retains_the_lease_after_its_coordinator_releases_it(self):
        with patch.object(storage, "PRODUCT_BYTES", 0):
            child = None
            try:
                with storage.product("child-build", root=self.root) as entry:
                    (entry / "output").write_bytes(b"active")
                    child = subprocess.Popen([sys.executable, "-c", "import sys; sys.stdin.read()"],
                                             stdin=subprocess.PIPE, **storage.subprocess_options())
                self.assertTrue(entry.exists(), "a child still using build products must protect them after the parent releases")
                child.communicate(timeout=10)
                storage.prune(self.root, 0)
                self.assertFalse(entry.exists(), "the child's exit must make its products reclaimable")
            finally:
                if child is not None and child.poll() is None:
                    child.kill()
                    child.communicate(timeout=10)

    def test_concurrent_tool_readers_keep_the_installation_leased(self):
        with storage.product("tools", root=self.root) as installed:
            (installed / "tool").write_bytes(b"executable")
        with patch.object(storage, "PRODUCT_BYTES", 0):
            with storage.product("tools", root=self.root, read_only=True) as first:
                program = "import importlib.util,sys; s=importlib.util.spec_from_file_location('storage',sys.argv[1]); m=importlib.util.module_from_spec(s); s.loader.exec_module(m)\nwith m.product('tools',root=sys.argv[2],read_only=True) as directory: print(directory)"
                second = subprocess.check_output([sys.executable, '-c', program, str(Path(storage.__file__)), str(self.root)],
                                                 text=True, timeout=15).strip()
                self.assertEqual(first, Path(second), "another development command must acquire tools while the first is still running")
                self.assertTrue(first.exists(), "one command finishing must not evict another command's tools")
            self.assertFalse(installed.exists(), "the final reader's exit must enforce the shared budget")

    def test_missing_tool_readers_do_not_create_installations(self):
        with storage.product("missing-tools", root=self.root, read_only=True) as directory:
            self.assertIsNone(directory, "missing tools must remain distinguishable from a completed installation")
        self.assertEqual(list(self.root.glob("*/.lease")), [], "read-only lookup must not accumulate empty installations")

    def test_dry_run_reports_reclaimable_entries_without_removing_them(self):
        old = self.run_output(1)
        planned = storage.prune(self.root, 0, dry_run=True)
        self.assertEqual([path for path, _ in planned], [old],
                         "dry run must report the same owned entries as cleanup")
        self.assertTrue((old / "build.log").exists(), "dry run must preserve all output bytes")

    def test_product_acquisition_refuses_to_adopt_unowned_files(self):
        import hashlib
        directory = self.root / hashlib.sha256(b"unowned").hexdigest()
        directory.mkdir()
        (directory / "customer").write_bytes(b"keep")
        with self.assertRaises(ValueError):
            with storage.product("unowned", root=self.root):
                self.fail("an unowned directory must not be leased")
        self.assertTrue((directory / "customer").exists(), "ownership rejection must preserve existing files")

    def test_worktree_intermediates_are_isolated_and_automatically_reclaimed(self):
        with patch.object(storage, 'shared_root', return_value=self.root / 'shared'), patch.object(storage, 'PRODUCT_BYTES', 0):
            with storage.development(self.root / 'a', {}) as first:
                entry = Path(first['ISOPLANE_BUILD_DIRECTORY'])
                for name in ('target/debug', 'target/native-support', 'managed/obj', 'runtime-work/compiler'):
                    output = entry / name
                    output.mkdir(parents=True, exist_ok=True)
                    (output / 'intermediate').write_bytes(b'generated')
                with storage.development(self.root / 'b', first) as second:
                    self.assertNotEqual(first['CARGO_TARGET_DIR'], second['CARGO_TARGET_DIR'],
                                        'entering another worktree must not share mutable Cargo outputs')
                    storage.prune(self.root / 'shared', 0)
                    self.assertTrue(entry.exists(), 'another worktree must not evict active build intermediates')
                self.assertTrue(entry.exists(), 'active work remains protected when another worktree finishes')
            self.assertFalse(entry.exists(), 'finishing a command must enforce retention on all owned intermediates')

    def test_explicit_cargo_paths_are_not_adopted_for_cleanup(self):
        target = self.root / 'caller-target'
        target.mkdir()
        (target / 'keep').touch()
        inherited = {'CARGO_TARGET_DIR': str(target), 'CARGO_BUILD_BUILD_DIR': str(target / 'build')}
        with patch.object(storage, 'shared_root', return_value=self.root / 'shared'), patch.object(storage, 'PRODUCT_BYTES', 0):
            with storage.development(self.root, inherited) as environment:
                self.assertEqual(environment['CARGO_TARGET_DIR'], inherited['CARGO_TARGET_DIR'],
                                 'explicit Cargo paths must remain caller-managed')
            self.assertTrue((target / 'keep').exists(), 'automatic retention must not adopt explicit output paths')

    def test_managed_builds_put_outputs_and_intermediates_under_the_command_lease(self):
        project = self.root / 'source'
        project.mkdir()
        shutil.copyfile(Path(__file__).parents[2] / 'Directory.Build.props', project / 'Directory.Build.props')
        (project / 'Probe.csproj').write_text('<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup></Project>')
        (project / 'Probe.cs').write_text('public class Probe {}')
        with patch.object(storage, 'shared_root', return_value=self.root / 'shared'), patch.object(storage, 'PRODUCT_BYTES', 0):
            with storage.development(project, os.environ) as environment:
                result = subprocess.run(['dotnet', 'build', '--disable-build-servers', '-p:NuGetAudit=false'],
                                        cwd=project, env=environment, capture_output=True, text=True, timeout=90)
                self.assertEqual(result.returncode, 0, 'the managed build must succeed: ' + result.stdout + result.stderr)
                directory = Path(environment['ISOPLANE_BUILD_DIRECTORY'])
                self.assertTrue(list((directory / 'managed/bin').rglob('Probe.dll')), 'the managed assembly must be inside retained storage')
                self.assertTrue(list((directory / 'managed/obj').rglob('project.assets.json')), 'restore intermediates must be inside retained storage')
                self.assertFalse((project / 'obj').exists(), 'repository builds must not leave unbounded per-project intermediates')
            self.assertFalse(directory.exists(), 'completed managed builds must obey automatic retention')

    def test_runtime_toolchain_extraction_uses_retained_work_storage(self):
        project = self.root / 'source'
        runtime = project / 'guest-runtime'
        runtime.mkdir(parents=True)
        shutil.copyfile(Path(__file__).parents[2] / 'guest-runtime/build.sh', runtime / 'build.sh')
        content = self.root / 'sdk/bin'
        content.mkdir(parents=True)
        (content / 'clang').write_bytes(b'compiler fixture')
        archive = self.root / 'sdk.tar.gz'
        with tarfile.open(archive, 'w:gz') as output:
            output.add(content.parent, arcname='sdk')
        (runtime / 'manifest.env').write_text('WASI_SDK_VERSION=test\nWASI_SDK_SHA256=' + storage.digest(archive)
                                            + '\nWASI_SDK_WINDOWS_SHA256=' + storage.digest(archive)
                                            + '\nWASI_SDK_WINDOWS_URL=unused\n')
        with patch.object(storage, 'shared_root', return_value=self.root / 'shared'), patch.object(storage, 'PRODUCT_BYTES', 0):
            with storage.development(project, os.environ) as environment:
                work = Path(environment['ISOPLANE_RUNTIME_WORK'])
                (work / 'downloads').mkdir(parents=True)
                host = 'windows' if os.name == 'nt' else 'linux'
                shutil.copyfile(archive, work / f'downloads/wasi-sdk-test-x86_64-{host}.tar.gz')
                result = subprocess.run(['bash', str(runtime / 'build.sh'), 'toolchain'], env=environment,
                                        capture_output=True, text=True, timeout=30)
                self.assertEqual(result.returncode, 0, 'the runtime stage must succeed: ' + result.stdout + result.stderr)
                self.assertEqual((work / 'wasi-sdk/bin/clang').read_bytes(), b'compiler fixture',
                                 'the runtime builder must extract into owned work storage')
                self.assertFalse((runtime / 'work').exists(), 'runtime intermediates must not accumulate under the worktree')
            self.assertFalse(work.exists(), 'runtime build intermediates must obey automatic retention')

    def test_managed_projects_with_the_same_name_keep_separate_outputs(self):
        project = self.root / 'source'
        project.mkdir()
        shutil.copyfile(Path(__file__).parents[2] / 'Directory.Build.props', project / 'Directory.Build.props')
        with patch.object(storage, 'shared_root', return_value=self.root / 'shared'):
            with storage.development(project, os.environ) as environment:
                for name in ('first', 'second'):
                    directory = project / name
                    directory.mkdir()
                    (directory / 'Probe.csproj').write_text('<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup></Project>')
                    (directory / 'Probe.cs').write_text('public class ' + name + ' {}')
                    result = subprocess.run(['dotnet', 'build', '--disable-build-servers', '-p:NuGetAudit=false'],
                                            cwd=directory, env=environment, capture_output=True, text=True, timeout=90)
                    self.assertEqual(result.returncode, 0, 'both managed projects must build: ' + result.stdout + result.stderr)
                managed = Path(environment['ISOPLANE_BUILD_DIRECTORY']) / 'managed'
                assemblies = list(managed.glob('*/bin/Probe/debug/Probe.dll'))
                self.assertEqual(len(assemblies), 2, 'same-named projects must not overwrite one another in managed storage')
                self.assertNotEqual(assemblies[0].read_bytes(), assemblies[1].read_bytes(),
                                    'the build products must retain each project\'s own code')


if __name__ == "__main__":
    unittest.main()
