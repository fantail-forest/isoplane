"""Build the ASP.NET fixture as a fresh NuGet SDK consumer."""
from pathlib import Path
import os
import shutil
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[2]
GUEST = Path(__file__).resolve().parent


def run(*args, **kwargs):
    subprocess.run(args, check=True, **kwargs)


def verify_missing_factory_fails(source, environment):
    result = subprocess.run(["dotnet", "build", str(source / "AspNet.csproj"),
                             "-p:WispApplicationFactory=", "--disable-build-servers"],
                            env=environment, capture_output=True, text=True)
    if result.returncode == 0 or "WISP0079" not in result.stdout + result.stderr:
        raise AssertionError("A missing application factory must fail the build with WISP0079:\n"
                             + result.stdout + result.stderr)


def verify_publication_refusal(source, environment, target, diagnostic, reason):
    control = source / "Directory.Build.targets"
    control.write_text(target)
    try:
        result = subprocess.run(["dotnet", "publish", str(source / "AspNet.csproj"),
                                 "-c", "Release", "--disable-build-servers"],
                                env=environment, capture_output=True, text=True)
        if result.returncode == 0 or diagnostic not in result.stdout + result.stderr:
            raise AssertionError(reason + "\n" + result.stdout + result.stderr)
    finally:
        control.unlink()


def verify_missing_framework_fails(source, environment):
    verify_publication_refusal(source, environment, '''<Project>
  <Target Name="RemoveFrameworkRuntimeForNegativeControl" AfterTargets="WispAspNetCoreReferences">
    <ItemGroup><IlcReference Remove="@(_WispAspNetCoreAssembly)" /></ItemGroup>
  </Target>
</Project>
''', ": AOT analysis error IL1005:",
        "Unresolvable framework methods must fail publication with IL1005.")


def verify_missing_sdk_fails(source, environment):
    verify_publication_refusal(source, environment, '''<Project>
  <Target Name="RemoveSdkForNegativeControl" BeforeTargets="WispAdaptRuntime">
    <ItemGroup><IlcReference Remove="@(IlcReference)" Condition="'%(Filename)' == 'Wisp.Sdk'" /></ItemGroup>
  </Target>
</Project>
''', "error WISP0080:",
        "A missing native SDK reference must fail with WISP0080 before runtime adaptation.")


def main():
    with tempfile.TemporaryDirectory(prefix="wisp-aspnet-") as temporary:
        scratch = Path(temporary)
        packages = scratch / "packages"
        for project in ["Wisp.Sdk.csproj", "AspNetCore/Wisp.AspNetCore.csproj"]:
            run("dotnet", "pack", str(ROOT / "guest-sdks/dotnet" / project),
                "--artifacts-path", str(scratch / "artifacts"), "-o", str(packages))
        config = ET.parse(ROOT / "nuget.config")
        ET.SubElement(config.getroot().find("packageSources"), "add",
                      key="fixture-packages", value=str(packages))
        mapping = ET.SubElement(config.getroot(), "packageSourceMapping")
        for entry in config.getroot().find("packageSources").findall("add"):
            package_source = ET.SubElement(mapping, "packageSource", key=entry.attrib["key"])
            ET.SubElement(package_source, "package", pattern="Wisp.*" if entry.attrib["key"] == "fixture-packages" else "*")
        config.write(scratch / "nuget.config")
        shutil.copy(ROOT / "global.json", scratch / "global.json")
        source = scratch / "app/src"
        source.mkdir(parents=True)
        shutil.copy(GUEST / "src/Application.cs", source / "Application.cs")
        shutil.copy(GUEST / "src/AspNet.csproj", source / "AspNet.csproj")
        environment = dict(os.environ, NUGET_PACKAGES=str(scratch / "nuget"),
                           WISP_OUT_DIR=os.environ.get("WISP_OUT_DIR", str(GUEST / "out")))
        verify_missing_factory_fails(source, environment)
        verify_missing_sdk_fails(source, environment)
        verify_missing_framework_fails(source, environment)
        run(str(ROOT / "guest-sdks/dotnet/build-guest.sh"), str(source.parent),
            *sys.argv[1:], env=environment, cwd=scratch)


if __name__ == "__main__":
    main()
