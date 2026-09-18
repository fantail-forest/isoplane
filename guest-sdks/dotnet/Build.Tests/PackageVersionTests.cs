using System.Diagnostics;
using System.IO.Compression;
using System.Runtime.CompilerServices;
using System.Xml.Linq;
using Xunit;

public sealed class PackageVersionTests : IDisposable
{
    private readonly string directory = Directory.CreateTempSubdirectory("isoplane-package-version-").FullName;
    private static string SdkDirectory([CallerFilePath] string source = "") => Path.GetFullPath(Path.Combine(Path.GetDirectoryName(source)!, ".."));

    [Fact]
    public async Task successive_package_versions_pin_their_own_sdk_and_template_dependencies()
    {
        foreach (var version in new[] { "0.1.0-package-test.1", "0.1.0-package-test.2" })
        {
            await Pack("Isoplane.Sdk.csproj", version);
            await Pack("AspNetCore/Isoplane.AspNetCore.csproj", version);
            await Pack("Templates/Isoplane.Templates.csproj", version);
            using var core = ZipFile.OpenRead(Path.Combine(directory, "Isoplane.Sdk." + version + ".nupkg"));
            var coreProps = ReadXml(core, "Sdk/Sdk.props");
            Assert.True(coreProps.Descendants("PackageReference").Single(element => (string?)element.Attribute("Include") == "Isoplane.Sdk").Attribute("Version")?.Value == version,
                "The core project SDK must pin its own runtime API package version.");
            using var sdk = ZipFile.OpenRead(Path.Combine(directory, "Isoplane.AspNetCore." + version + ".nupkg"));
            var props = ReadXml(sdk, "Sdk/Sdk.props");
            Assert.True(props.Descendants("PackageReference").Single(element => (string?)element.Attribute("Include") == "Isoplane.AspNetCore").Attribute("Version")?.Value == version,
                "The packaged SDK must reference its own package version rather than a cached earlier release.");
            var nuspec = ReadXml(sdk, "Isoplane.AspNetCore.nuspec");
            Assert.True(nuspec.Descendants().Single(element => element.Name.LocalName == "dependency" && (string?)element.Attribute("id") == "Isoplane.Sdk").Attribute("version")?.Value == version,
                "The ASP.NET package must depend on the matching core SDK release.");
            using var templates = ZipFile.OpenRead(Path.Combine(directory, "Isoplane.Templates." + version + ".nupkg"));
            var project = ReadXml(templates, "content/content/aspnet/SampleApp.csproj");
            Assert.True(project.Root?.Attribute("Sdk")?.Value == "Isoplane.AspNetCore/" + version,
                "An installed template must create a project using the matching SDK release.");
        }
    }

    private async Task Pack(string project, string version)
    {
        var start = new ProcessStartInfo("dotnet") { UseShellExecute = false, RedirectStandardOutput = true, RedirectStandardError = true };
        foreach (var argument in new[] { "pack", Path.Combine(SdkDirectory(), project), "-c", "Release", "--artifacts-path", Path.Combine(directory, "artifacts"), "-o", directory, "--disable-build-servers", "-p:Version=" + version })
            start.ArgumentList.Add(argument);
        using var process = Process.Start(start)!;
        var output = process.StandardOutput.ReadToEndAsync();
        var error = process.StandardError.ReadToEndAsync();
        using var timeout = new CancellationTokenSource(TimeSpan.FromMinutes(2));
        try
        {
            await process.WaitForExitAsync(timeout.Token);
            Assert.True(process.ExitCode == 0, "Packing the actual SDK/template must succeed: " + await output + await error);
        }
        finally
        {
            if (!process.HasExited) process.Kill(entireProcessTree: true);
            await process.WaitForExitAsync();
        }
    }

    private static XDocument ReadXml(ZipArchive package, string path)
    {
        var entry = package.GetEntry(path);
        Assert.True(entry is not null, "The package must contain " + path);
        using var content = entry.Open();
        return XDocument.Load(content);
    }

    public void Dispose() => Directory.Delete(directory, recursive: true);
}
