"""Build the ASP.NET fixture as a fresh NuGet SDK consumer."""
from pathlib import Path
import os
import re
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
                             "-p:IsoplaneApplicationFactory=", "--disable-build-servers"],
                            env=environment, capture_output=True, text=True)
    if result.returncode == 0 or "ISOPLANE0079" not in result.stdout + result.stderr:
        raise AssertionError("A missing application factory must fail the build with ISOPLANE0079:\n"
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
  <Target Name="RemoveFrameworkRuntimeForNegativeControl" AfterTargets="ComputeIlcCompileInputs">
    <ItemGroup><IlcReference Remove="@(_IsoplaneAspNetAssembly)" MatchOnMetadata="Filename;Extension" MatchOnMetadataOptions="CaseInsensitive" /></ItemGroup>
  </Target>
</Project>
''', ": AOT analysis error IL1005:",
        "Unresolvable framework methods must fail publication with IL1005.")


def main():
    with tempfile.TemporaryDirectory(prefix="isoplane-aspnet-") as temporary:
        scratch = Path(temporary)
        packages = scratch / "packages"
        for project in ["Isoplane.Sdk.csproj", "AspNetCore/Isoplane.AspNetCore.csproj"]:
            run("dotnet", "pack", str(ROOT / "guest-sdks/dotnet" / project),
                "--artifacts-path", str(scratch / "artifacts"), "-o", str(packages))
        # An installed package names the guest runtime feed the way a customer
        # does; here it is this checkout's, mapped by the ids it offers
        # (`<id>.<version>.nupkg`, the version being the first dotted segment
        # that starts with a digit).
        feed = Path(os.environ.get("ISOPLANE_GUEST_RUNTIME_FEED", str(ROOT / "guest-runtime/out/feed")))
        local_feeds = {"isoplane-guest-runtime": feed}
        compiler = feed.parent / "compiler"
        if compiler.is_dir():
            local_feeds["isoplane-guest-compiler"] = compiler
        config = ET.parse(ROOT / "nuget.config")
        ET.SubElement(config.getroot().find("packageSources"), "add",
                      key="fixture-packages", value=str(packages))
        for key, directory in local_feeds.items():
            ET.SubElement(config.getroot().find("packageSources"), "add",
                          key=key, value=str(directory))
        mapping = ET.SubElement(config.getroot(), "packageSourceMapping")
        for entry in config.getroot().find("packageSources").findall("add"):
            package_source = ET.SubElement(mapping, "packageSource", key=entry.attrib["key"])
            if entry.attrib["key"] in local_feeds:
                feed_ids = sorted({re.match(r"^(.*?)\.\d[^/]*\.nupkg$", name.name).group(1)
                                   for name in local_feeds[entry.attrib["key"]].glob("*.nupkg")})
                for feed_id in feed_ids:
                    ET.SubElement(package_source, "package", pattern=feed_id)
            else:
                ET.SubElement(package_source, "package", pattern="Isoplane.*" if entry.attrib["key"] == "fixture-packages" else "*")
        config.write(scratch / "nuget.config")
        shutil.copy(ROOT / "global.json", scratch / "global.json")
        source = scratch / "app/src"
        source.mkdir(parents=True)
        shutil.copy(GUEST / "src/Application.cs", source / "Application.cs")
        shutil.copy(GUEST / "src/AspNet.csproj", source / "AspNet.csproj")
        environment = dict(os.environ, NUGET_PACKAGES=str(scratch / "nuget"),
                           ISOPLANE_OUT_DIR=os.environ.get("ISOPLANE_OUT_DIR", str(GUEST / "out")),
                           ISOPLANE_GUEST_RUNTIME_FEED=str(feed))
        verify_missing_factory_fails(source, environment)
        verify_missing_framework_fails(source, environment)
        run(str(ROOT / "guest-sdks/dotnet/build-guest.sh"), str(source.parent),
            *sys.argv[1:], env=environment, cwd=scratch)


if __name__ == "__main__":
    main()
