"""Exercise publication of the platform Mono runtime."""

import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[2]
DOTNET = shutil.which("dotnet")


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
            ET.SubElement(project, "Target", Name="IsoplaneMonoVerifyRuntimePack")
            path = root / "Publish.proj"
            ET.ElementTree(project).write(path)
            result = subprocess.run([DOTNET, "msbuild", str(path), "-t:IsoplaneMonoPublishCore", "-getProperty:NativeBinary", "-nologo"],
                                    capture_output=True, text=True, timeout=30)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            module = root / "output directory/Application.wasm"
            self.assertEqual(module.read_bytes(), source.read_bytes(),
                             "the linked module must land inside the requested output directory")
            self.assertEqual(module.with_suffix(".wasm.runtime").read_text().strip(),
                             "1.0.0@sha512:fixture-digest", "the runtime identity must accompany the module")


if __name__ == "__main__":
    unittest.main()
