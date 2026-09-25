"""Exercise MSBuild's container boundary with a simulated Docker daemon."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[2]
DOTNET = shutil.which("dotnet")


class ContainerPublishTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="isoplane publish '")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.bin = self.root / "tools"
        self.bin.mkdir()
        self.env = dict(os.environ, PATH=str(self.bin), FAKE_ROOT=str(self.root))
        for command in ["dotnet", "id", "sh"]:
            executable = shutil.which(command)
            self.assertIsNotNone(executable, f"install {command} to test the MSBuild integration")
            (self.bin / command).symlink_to(executable)
        feed = self.root / "runtime-feed"
        sdk = self.root / "wasi-sdk"
        feed.mkdir()
        (sdk / "Sdk").mkdir(parents=True)
        (sdk / "Sdk/Sdk.targets").write_text("<Project />")
        props = ET.Element("Project")
        group = ET.SubElement(props, "PropertyGroup")
        for name, value in {
            "IsoplaneGuestRuntimePackVersion": "1.0.0",
            "IsoplaneGuestNativeAotPackVersion": "1.0.0",
            "IsoplaneGuestWasiSdkPackDirectory": str(sdk) + "/",
        }.items():
            ET.SubElement(group, name).text = value
        ET.ElementTree(props).write(feed / "Isoplane.GuestRuntime.props")
        self.env["ISOPLANE_GUEST_RUNTIME_FEED"] = str(feed)
        validator = self.bin / "isoplane-validator"
        validator.write_text(f"#!{sys.executable}\n" + """
import json, os, pathlib, sys
root = pathlib.Path(os.environ['FAKE_ROOT'])
assert sys.argv[1] == 'doctor', 'container output must pass the normal validator entry point'
with (root / 'validator-calls').open('a') as log:
    log.write(json.dumps(sys.argv[1:]) + '\\n')
assert pathlib.Path(sys.argv[2]).read_bytes() == b'published module', 'validate the actual compiler result'
sys.exit(2 if os.environ.get('FAKE_MODE') == 'validator-rejection' else 0)
""")
        validator.chmod(0o755)
        self.env["ISOPLANE_CLI"] = str(validator)
        self.env["NUGET_PACKAGES"] = str(self.root / "shared-nuget")
        self.env["NUGET_SCRATCH"] = str(self.root / "nuget-scratch")
        self.project = self.root / "Guest.csproj"
        self.project.write_text(f'''<Project Sdk="Microsoft.NET.Sdk">
<Import Project="{ROOT}/guest-sdks/dotnet/Isoplane.Guest.props" />
</Project>''')
        docker = self.bin / "docker"
        docker.write_text(f"#!{sys.executable}\n" + '''
import json, os, pathlib, sys
root = pathlib.Path(os.environ['FAKE_ROOT'])
args = sys.argv[1:]
with (root / 'calls').open('a') as log:
    log.write(json.dumps(args) + '\\n')
mode = os.environ.get('FAKE_MODE', '')
if args[0] == 'info':
    sys.exit(1 if mode == 'stopped' else 0)
elif args[:2] == ['image', 'inspect']:
    sys.exit(1 if mode == 'missing-image' else 0)
elif args[0] == 'create':
    (root / 'created').write_text(json.dumps(args))
    print('owned-container')
elif args[0] == 'start':
    if mode == 'compiler-error':
        print('Handler.cs(1,1): error CS1002: ; expected')
        sys.exit(23)
    saved = json.loads((root / 'created').read_text())
    scratch = pathlib.Path(next(a[:-7] for a in saved if a.endswith(':/build')))
    output = scratch / 'publish'
    output.mkdir()
    if mode != 'missing-module':
        (output / 'guest.wasm').write_bytes(b'published module')
        (output / 'guest.wasm.runtime').write_text('test-runtime@sha512:test-digest')
elif args[0] == 'ps':
    if (root / 'created').exists(): print('owned-container')
elif args[0] == 'rm':
    if mode == 'cleanup-error':
        print('daemon unavailable during cleanup')
        sys.exit(1)
    assert args == ['rm', '--force', 'owned-container'], 'must remove only our labelled container'
    (root / 'removed').write_text('removed')
else:
    raise AssertionError('unexpected Docker operation: ' + repr(args))
''')
        docker.chmod(0o755)

    def publish(self, *properties):
        self.assertIsNotNone(DOTNET, "install the .NET SDK to test the MSBuild integration")
        return subprocess.run([
            DOTNET, "msbuild", str(self.project), "-t:Publish", "-nologo",
            "-p:IsoplaneContainerBuild=true",
            "-p:Configuration=Release", f"-p:PublishDir={self.root}/out",
            f"-p:IsoplaneCompilerCache={self.root}/cache",
            *properties,
        ], env=self.env, cwd=self.root, capture_output=True, text=True, timeout=90)

    def test_build_intermediates_stay_inside_the_project(self):
        result = self.publish("-p:IsoplaneCompilerCache=")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        args = json.loads((self.root / "created").read_text())
        scratch = Path(next(a[:-7] for a in args if a.endswith(":/build")))
        self.assertTrue(scratch.is_relative_to(self.root / ".isoplane"),
                        "compiler intermediates must stay inside the project")
        self.assertFalse(scratch.exists(), "completed publication must remove its scratch directory")
        cache = Path(next(a.removeprefix("NUGET_PACKAGES=") for a in args if a.startswith("NUGET_PACKAGES=")))
        self.assertEqual(cache, Path(self.env["NUGET_PACKAGES"]),
                         "container publication must reuse the selected standard NuGet cache")

    def test_a_missing_docker_cli_names_the_prerequisite(self):
        (self.bin / "docker").unlink()
        result = self.publish()
        self.assertNotEqual(result.returncode, 0, "publication must fail without the container CLI")
        self.assertIn("Docker CLI is missing", result.stdout, result.stdout)

    def test_an_ide_build_produces_il_without_contacting_docker(self):
        self.assertIsNotNone(DOTNET, "install the .NET SDK to test native IL builds")
        (self.root / "Handler.cs").write_text("public class Handler {}")
        result = subprocess.run([
            DOTNET, "build", str(self.project), "-c", "Release", "--disable-build-servers",
            "-p:IsoplaneContainerBuild=true", "-p:NuGetAudit=false",
        ], env=self.env, cwd=self.root, capture_output=True, text=True, timeout=90)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue((self.root / "bin/Release/net10.0/Guest.dll").exists(), "an IDE build must produce an IL assembly")
        self.assertFalse((self.root / "calls").exists(), "an IDE build must not require a running Docker daemon")
        self.assertFalse(list(self.root.rglob("*.wasm")), "an IDE build must leave WASM compilation to publishing")

    def test_a_stopped_runtime_names_the_recovery(self):
        self.env["FAKE_MODE"] = "stopped"
        result = self.publish()
        self.assertNotEqual(result.returncode, 0, "publication must fail when Docker is stopped")
        self.assertIn("Install and start Docker Desktop", result.stdout, result.stdout)

    def test_offline_publication_requires_a_cached_image(self):
        self.env["FAKE_MODE"] = "missing-image"
        result = self.publish("-p:IsoplaneOffline=true")
        self.assertNotEqual(result.returncode, 0, "offline publication cannot build a missing image")
        self.assertIn("offline compiler image is missing", result.stdout, result.stdout)
        self.assertFalse((self.root / "created").exists(), "missing offline inputs must not start compilation")

    def test_compiler_failure_preserves_the_previous_module_and_reclaims_the_container(self):
        self.env["FAKE_MODE"] = "compiler-error"
        (self.root / "out").mkdir()
        module = self.root / "out/guest.wasm"
        module.write_bytes(b"working")
        result = self.publish()
        self.assertNotEqual(result.returncode, 0, "a compiler failure must fail MSBuild")
        self.assertIn("error CS1002", result.stdout, "compiler diagnostics must reach MSBuild")
        self.assertIn("exit 23", result.stdout, "the compiler exit code must reach MSBuild")
        self.assertEqual(module.read_bytes(), b"working", "a failed compiler must preserve the previous module")
        self.assertTrue((self.root / "removed").exists(), "a failed compilation must remove its container")

    def test_validator_rejection_preserves_previous_output_and_cleans_up(self):
        self.env["FAKE_MODE"] = "validator-rejection"
        (self.root / "out").mkdir()
        module = self.root / "out/guest.wasm"
        module.write_bytes(b"working")
        result = self.publish()
        self.assertNotEqual(result.returncode, 0, "a rejected compiler artifact must fail publication")
        self.assertIn("validating the linked module", result.stdout, result.stdout)
        self.assertTrue((self.root / "validator-calls").exists(), "publication must call the validator")
        self.assertEqual(module.read_bytes(), b"working", "rejection must preserve the prior valid module")
        self.assertTrue((self.root / "removed").exists(), "rejection must reclaim its compiler container")

    def test_offline_builds_preserve_arguments_isolate_outputs_and_disable_network(self):
        result = self.publish("-p:IsoplaneOffline=true", "-p:DefineConstants=FIRST%3BSECOND")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        args = json.loads((self.root / "created").read_text())
        self.assertIn(str(self.project), args, "spaces and quotes in project paths must survive process invocation")
        self.assertIn("-p:DefineConstants=FIRST%3BSECOND", args, "MSBuild property delimiters must survive forwarding")
        self.assertEqual(args[args.index("--network") + 1], "none", "offline compilation must have no network")
        self.assertIn("-p:ArtifactsPath=/build/artifacts", args, "Linux intermediates must be isolated from native build outputs")
        scratch = self.env["NUGET_SCRATCH"]
        self.assertIn("NUGET_SCRATCH=" + scratch, args, "host and container restores must use the same extraction locks")
        self.assertIn(scratch + ":" + scratch, args, "the host lock directory must be mounted at the same path")
        cache = str(self.root / "cache")
        self.assertIn(cache + ":" + cache, args, "NuGet must hash the same absolute package path in both environments")
        self.assertEqual((self.root / "out/guest.wasm").read_bytes(), b"published module", "MSBuild must return the compiler output")
        self.assertTrue((self.root / "removed").exists(), "successful publication must remove its container")

    def test_success_without_a_module_is_an_error(self):
        self.env["FAKE_MODE"] = "missing-module"
        result = self.publish()
        self.assertNotEqual(result.returncode, 0, "a compiler's success status alone does not prove publication")
        self.assertIn("exactly one WASM", result.stdout, result.stdout)
        self.assertTrue((self.root / "removed").exists(), "invalid compiler output must still release the container")

    def test_clean_removes_published_modules_without_docker_or_shared_cache_deletion(self):
        outputs = [self.root / "bin/Release/net10.0/publish", self.root / "custom output"]
        for output in outputs:
            result = self.publish(f"-p:PublishDir={output}")
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertTrue((output / "guest.wasm").exists(), "publish must produce the module before clean")
        unrelated = outputs[1] / "other-project.wasm"
        unrelated.write_bytes(b"another project's output")
        cache = self.root / "cache/keep-package"
        cache.write_bytes(b"cached package")
        (self.bin / "docker").unlink()
        for _ in range(2):
            result = subprocess.run([
                DOTNET, "clean", str(self.project), "-c", "Release",
                "-p:IsoplaneContainerBuild=true",
            ], env=self.env, cwd=self.root, capture_output=True, text=True, timeout=90)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            for output in outputs:
                self.assertFalse((output / "guest.wasm").exists(), "clean must remove every recorded publish output")
                self.assertFalse((output / "guest.wasm.runtime").exists(), "clean must remove the published runtime identity alongside its module")
        self.assertEqual(unrelated.read_bytes(), b"another project's output", "clean must preserve files this project did not produce")
        self.assertEqual(cache.read_bytes(), b"cached package", "clean must preserve shared compiler packages")

    def test_clean_preserves_other_configurations(self):
        for configuration in ("Debug", "Release"):
            result = self.publish(f"-p:Configuration={configuration}", f"-p:PublishDir={self.root}/{configuration}")
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        result = subprocess.run([
            DOTNET, "clean", str(self.project), "-c", "Debug", "-p:IsoplaneContainerBuild=true",
        ], env=self.env, cwd=self.root, capture_output=True, text=True, timeout=90)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse((self.root / "Debug/guest.wasm").exists(), "clean must remove the selected configuration")
        self.assertTrue((self.root / "Release/guest.wasm").exists(), "clean must preserve other configurations")

    def test_cleanup_failure_is_not_reported_as_a_successful_publish(self):
        self.env["FAKE_MODE"] = "cleanup-error"
        result = self.publish()
        args = json.loads((self.root / "created").read_text())
        scratch = Path(next(a[:-7] for a in args if a.endswith(":/build")))
        self.addCleanup(shutil.rmtree, scratch, True)
        self.assertNotEqual(result.returncode, 0, "publication must report a compiler container it could not reclaim")
        self.assertIn("compiler cleanup failed", result.stdout, result.stdout)

    def test_a_build_root_outside_the_project_is_rejected_before_startup(self):
        result = self.publish(f"-p:IsoplaneBuildRoot={self.root}/unrelated")
        self.assertNotEqual(result.returncode, 0, "the mounted source tree must contain the project")
        self.assertIn("IsoplaneBuildRoot must contain", result.stdout, result.stdout)
        self.assertFalse((self.root / "calls").exists(), "invalid static inputs must not acquire container resources")

    def test_control_timeouts_must_be_positive(self):
        result = self.publish("-p:IsoplaneContainerCommandTimeoutSeconds=0")
        self.assertNotEqual(result.returncode, 0, "an invalid control timeout must fail before starting resources")
        self.assertIn("must be positive", result.stdout, result.stdout)


class MonoPublicationTests(unittest.TestCase):
    def test_untrimmed_publication_excludes_the_prebuilt_runtime_module(self):
        with tempfile.TemporaryDirectory(prefix="isoplane-mono-publish-items-") as directory:
            project = ET.Element("Project")
            properties = ET.SubElement(project, "PropertyGroup")
            ET.SubElement(properties, "PublishTrimmed").text = "false"
            items = ET.SubElement(project, "ItemGroup")
            ET.SubElement(items, "ResolvedFileToPublish", Include="dotnet.wasm;Application.wasm;data.json")
            targets = ET.parse(ROOT / "guest-sdks/dotnet/Build/Isoplane.Mono.targets")
            project.append(targets.find("Target[@Name='IsoplaneMonoDropPrebuiltRuntime']"))
            ET.SubElement(project, "Target", Name="ComputeFilesToPublish")
            path = Path(directory) / "Publish.proj"
            ET.ElementTree(project).write(path)
            result = subprocess.run([DOTNET, "msbuild", str(path), "-t:ComputeFilesToPublish",
                                     "-getItem:ResolvedFileToPublish", "-nologo"],
                                    capture_output=True, text=True, timeout=30)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            files = [item["Identity"] for item in json.loads(result.stdout)["Items"]["ResolvedFileToPublish"]]
            self.assertEqual(files, ["Application.wasm", "data.json"],
                             "publication must retain application outputs and remove the unused runtime module without requiring trimming")

    def test_publication_accepts_output_directories_without_a_trailing_separator(self):
        with tempfile.TemporaryDirectory(prefix="isoplane-mono-publication-") as directory:
            root = Path(directory)
            source = root / "linked.wasm"
            source.write_bytes(b"linked guest")
            project = ET.Element("Project")
            properties = ET.SubElement(project, "PropertyGroup")
            for name, value in {
                "PublishDir": str(root / "output directory"),
                "AssemblyName": "Application",
                "_WasiOutputFileName": str(source),
                "IsoplaneGuestRuntimePackVersion": "1.0.0",
                "IsoplaneGuestRuntimePackSha512": "fixture-digest",
            }.items():
                ET.SubElement(properties, name).text = value
            targets = ET.parse(ROOT / "guest-sdks/dotnet/Build/Isoplane.Mono.targets")
            project.append(targets.find("Target[@Name='IsoplaneMonoPublishCore']"))
            run_targets = ET.parse(ROOT / "guest-sdks/dotnet/Build/Isoplane.Run.targets")
            project.append(run_targets.find("Target[@Name='ComputeRunArguments']"))
            ET.SubElement(project, "Target", Name="IsoplaneMonoVerifyRuntimePack")
            path = root / "Publish.proj"
            ET.ElementTree(project).write(path)
            result = subprocess.run([DOTNET, "msbuild", str(path), "-t:IsoplaneMonoPublishCore;ComputeRunArguments", "-getProperty:NativeBinary,RunArguments", "-nologo"],
                                    capture_output=True, text=True, timeout=30)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            module = root / "output directory/Application.wasm"
            arguments = json.loads(result.stdout)["Properties"]["RunArguments"]
            self.assertIn(f'--module "{module}"', arguments,
                          "dotnet run must select the module inside the same publication directory")
            self.assertEqual(module.read_bytes(), source.read_bytes(),
                             "the linked module must land inside the requested output directory")
            self.assertEqual(module.with_suffix(".wasm.runtime").read_text().strip(),
                             "1.0.0@sha512:fixture-digest", "the runtime identity must accompany the module")


if __name__ == "__main__":
    unittest.main()
