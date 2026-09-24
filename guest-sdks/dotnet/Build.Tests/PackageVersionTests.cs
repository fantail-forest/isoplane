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

    [Fact]
    public async Task the_sdk_package_carries_its_shared_runtime_and_what_it_was_built_from()
    {
        const string version = "0.1.0-package-test.3";
        await Pack("Isoplane.Sdk.csproj", version);
        using var package = ZipFile.OpenRead(Path.Combine(directory, "Isoplane.Sdk." + version + ".nupkg"));
        foreach (var (name, bytes) in RuntimeModules)
        {
            var module = package.GetEntry("tools/runtime/" + name);
            Assert.True(module is not null, "The SDK must carry each runtime interpreted applications run or debug on: " + name);
            using var content = module.Open();
            using var packed = new MemoryStream();
            await content.CopyToAsync(packed);
            Assert.True(packed.ToArray().SequenceEqual(bytes), "The SDK must carry the runtime it was packed with, unchanged: " + name);
        }
        var provenance = package.GetEntry("tools/runtime/runtime.json");
        Assert.True(provenance is not null, "The SDK must record what its runtime was built from.");
        using var json = await System.Text.Json.JsonDocument.ParseAsync(provenance.Open());
        var root = json.RootElement;
        Assert.True(root.GetProperty("sdkVersion").GetString() == version, "The runtime record must name the SDK release carrying it: " + root);
        Assert.True(root.GetProperty("sourceRevision").GetString() == "package-test-revision", "The runtime record must name the source the runtimes were built from: " + root);
        Assert.True(root.GetProperty("monoPack").GetProperty("version").GetString() == "10.0.11-package-test"
            && root.GetProperty("monoPack").GetProperty("sha512").GetString() == "cGFjaw==",
            "The runtime record must name the Mono pack the runtime linked: " + root);
        foreach (var (name, bytes) in RuntimeModules)
            Assert.True(root.GetProperty("modules").GetProperty(name).GetProperty("sha256").GetString()
                == Convert.ToHexStringLower(System.Security.Cryptography.SHA256.HashData(bytes)),
                "The runtime record must identify the module bytes the SDK carries: " + root);
    }

    [Fact]
    public async Task packing_the_sdk_without_its_shared_runtime_fails()
    {
        var (exit, output) = await TryPack("Isoplane.Sdk.csproj", "0.1.0-package-test.4", runtime: null);
        Assert.True(exit != 0 && output.Contains("IsoplaneRuntimeModuleDirectory", StringComparison.Ordinal),
            "An SDK without its shared runtime cannot run interpreted applications, so packing one must fail: " + output);
    }

    [Fact]
    public async Task packing_the_sdk_without_its_local_debug_runtime_fails()
    {
        const string version = "0.1.0-package-test.5";
        var runtime = RuntimeDirectory(version);
        File.Delete(Path.Combine(runtime, "Isoplane.MonoRuntime.Debug.wasm"));
        var (exit, output) = await TryPack("Isoplane.Sdk.csproj", version, runtime);
        Assert.True(exit != 0 && output.Contains("Isoplane.MonoRuntime.Debug.wasm", StringComparison.Ordinal),
            "An SDK without its local-debug runtime cannot debug interpreted applications, so packing one must fail: " + output);
    }

    [Fact]
    public async Task packing_the_sdk_with_runtimes_built_for_another_release_fails()
    {
        var (exit, output) = await TryPack("Isoplane.Sdk.csproj", "0.1.0-package-test.6", RuntimeDirectory("0.1.0-package-test.0"));
        Assert.True(exit != 0 && output.Contains("were built for SDK 0.1.0-package-test.0", StringComparison.Ordinal),
            "Runtimes cached from another release must not ship in this one: " + output);
    }

    [Fact]
    public async Task packing_a_release_with_runtimes_built_from_other_source_fails()
    {
        const string version = "0.1.0-package-test.7";
        var (exit, output) = await TryPack("Isoplane.Sdk.csproj", version, RuntimeDirectory(version), "-p:ContinuousIntegrationBuild=true", "-p:SourceRevisionId=release-revision");
        Assert.True(exit != 0 && output.Contains("were built from package-test-revision", StringComparison.Ordinal),
            "A release must ship runtimes built from its own source revision: " + output);
    }

    [Fact]
    public async Task packing_the_sdk_with_a_runtime_its_receipt_does_not_record_fails()
    {
        const string version = "0.1.0-package-test.8";
        var runtime = RuntimeDirectory(version);
        File.WriteAllBytes(Path.Combine(runtime, "Isoplane.MonoRuntime.wasm"), [0x00, 0x61, 0x73, 0x6d, 0x02, 0x00, 0x00, 0x00]);
        var (exit, output) = await TryPack("Isoplane.Sdk.csproj", version, runtime);
        Assert.True(exit != 0 && output.Contains("Isoplane.MonoRuntime.wasm is not the module runtime-build.json records", StringComparison.Ordinal),
            "A module replaced after its build must not ship under that build's receipt: " + output);
    }

    private static readonly (string Name, byte[] Bytes)[] RuntimeModules =
    [
        ("Isoplane.MonoRuntime.wasm", [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]),
        ("Isoplane.MonoRuntime.Debug.wasm", [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x00]),
    ];

    /// <summary>
    /// A stand-in for the output of MonoRuntime/build.sh for SDK <paramref name="version"/>;
    /// packing only checks, copies and records it.
    /// </summary>
    private string RuntimeDirectory(string version)
    {
        var runtime = Path.Combine(directory, "runtime-" + version);
        Directory.CreateDirectory(runtime);
        var modules = new System.Text.Json.Nodes.JsonObject();
        foreach (var (name, bytes) in RuntimeModules)
        {
            File.WriteAllBytes(Path.Combine(runtime, name), bytes);
            modules[name] = Convert.ToHexStringLower(System.Security.Cryptography.SHA256.HashData(bytes));
        }
        var receipt = new System.Text.Json.Nodes.JsonObject
        {
            ["sdkVersion"] = version,
            ["sourceRevision"] = "package-test-revision",
            ["monoPack"] = "10.0.11-package-test@sha512:cGFjaw==",
            ["modules"] = modules,
        };
        File.WriteAllText(Path.Combine(runtime, "runtime-build.json"), receipt.ToJsonString() + "\n");
        return runtime;
    }

    private async Task Pack(string project, string version)
    {
        var (exit, output) = await TryPack(project, version, RuntimeDirectory(version));
        Assert.True(exit == 0, "Packing the actual SDK/template must succeed: " + output);
    }

    private async Task<(int Exit, string Output)> TryPack(string project, string version, string? runtime, params string[] properties)
    {
        var start = new ProcessStartInfo("dotnet") { UseShellExecute = false, RedirectStandardOutput = true, RedirectStandardError = true };
        foreach (var argument in new[] { "pack", Path.Combine(SdkDirectory(), project), "-c", "Release", "--artifacts-path", Path.Combine(directory, "artifacts"), "-o", directory, "--disable-build-servers", "-p:Version=" + version })
            start.ArgumentList.Add(argument);
        if (runtime is not null)
            start.ArgumentList.Add("-p:IsoplaneRuntimeModuleDirectory=" + runtime);
        foreach (var property in properties)
            start.ArgumentList.Add(property);
        start.Environment.Remove("ISOPLANE_RUNTIME_MODULE_DIRECTORY");
        using var process = Process.Start(start)!;
        var output = process.StandardOutput.ReadToEndAsync();
        var error = process.StandardError.ReadToEndAsync();
        using var timeout = new CancellationTokenSource(TimeSpan.FromMinutes(2));
        try
        {
            await process.WaitForExitAsync(timeout.Token);
            return (process.ExitCode, await output + await error);
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
