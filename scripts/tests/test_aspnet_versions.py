"""Exercise application framework selection through restore, compilation and bundling."""
import itertools
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
ASPNET = ROOT / "guest-sdks/dotnet/AspNetCore/buildTransitive"


class AspNetVersionTests(unittest.TestCase):
    def test_application_patch_selection_survives_restore_compile_and_bundle(self):
        for version, profile in itertools.product(("10.0.11", "10.0.12"), ("Mono", "NativeAOT")):
            with self.subTest(version=version, profile=profile), tempfile.TemporaryDirectory(prefix="wisp-aspnet-version-") as directory:
                root = Path(directory)
                project = root / "Application.csproj"
                project.write_text(f'''<Project Sdk="Microsoft.NET.Sdk.Web">
  <Import Project="{ASPNET}/Wisp.AspNetCore.props" />
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
    <RuntimeFrameworkVersion>{version}</RuntimeFrameworkVersion>
    <WispGuestRuntime>{profile}</WispGuestRuntime>
    <WispManagedBundleBuild>true</WispManagedBundleBuild>
    <WispApplicationFactory>Application.Create</WispApplicationFactory>
    <OutputType>Library</OutputType>
    <CopyLocalLockFileAssemblies>true</CopyLocalLockFileAssemblies>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="Microsoft.Extensions.Logging.Abstractions" Version="10.0.11" />
  </ItemGroup>
  <Import Project="{ASPNET}/Wisp.AspNetCore.targets" />
  <Import Project="{ROOT}/guest-sdks/dotnet/Build/Wisp.ManagedBundle.targets" />
</Project>''')
                (root / "Application.cs").write_text('''using Microsoft.AspNetCore.Builder;
public static class Application {
    public static WebApplication Create() {
        var app = WebApplication.CreateBuilder().Build();
        app.MapGet("/", () => "hello");
        return app;
    }
}''')
                result = subprocess.run([
                    "dotnet", "msbuild", str(project), "-restore", "-t:WispManagedBundle",
                    "-getProperty:WispAspNetCoreRuntimeVersion",
                    "-getItem:Analyzer,WispBundleAssembly,WispBundlePlatformReference",
                    "-nologo", "-p:NuGetAudit=false",
                ], cwd=root, env=dict(os.environ, DOTNET_RUNTIME_ID="linux-x64"),
                    text=True, capture_output=True, timeout=180)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                data = json.loads(result.stdout[result.stdout.index('{'):])
                self.assertEqual(data['Properties']['WispAspNetCoreRuntimeVersion'], version,
                                 "the application must own its framework patch selection")
                items = data['Items']
                generators = [item['Identity'] for item in items['Analyzer']
                              if item['Identity'].endswith('Microsoft.AspNetCore.Http.RequestDelegateGenerator.dll')]
                self.assertEqual(len(generators), 1, "the build must select exactly one request delegate generator")
                self.assertIn(f'/microsoft.aspnetcore.app.ref/{version}/', generators[0],
                              "the generator must come from the selected restored package")
                implementations = [item['Identity'] for item in items['WispBundleAssembly']
                                   if item['Identity'].endswith('/Microsoft.AspNetCore.dll')]
                self.assertEqual(len(implementations), 1, "deployment must carry the application's ASP.NET implementation")
                self.assertIn(f'/microsoft.aspnetcore.app.runtime.linux-x64/{version}/', implementations[0],
                              "bundling must preserve the selected patch bytes")
                libraries = [item['Identity'] for item in items['WispBundleAssembly']
                             if item['Identity'].endswith('/Microsoft.Extensions.Logging.Abstractions.dll')]
                self.assertEqual(len(libraries), 1, "a package reference must contribute only its resolved implementation")
                self.assertIn('/microsoft.extensions.logging.abstractions/10.0.11/', libraries[0],
                              "an explicit library version must not be overwritten by the ASP.NET runtime pack")
                self.assertFalse(any(item['Identity'].endswith('/Microsoft.AspNetCore.dll')
                                     for item in items['WispBundlePlatformReference']),
                                 "ASP.NET must not be replaced by the publisher's platform libraries")


if __name__ == '__main__':
    unittest.main()
