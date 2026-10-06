using Isoplane.TestSupport;
using System.Diagnostics;
using System.Formats.Tar;
using System.Text.Json;
using Xunit;
using Isoplane.Build;

namespace Isoplane.Cli.Tests;

public sealed class ManagedBundleTests : IClassFixture<ManagedBundleFixture>
{
    private readonly ManagedBundleFixture fixture;
    public ManagedBundleTests(ManagedBundleFixture fixture) => this.fixture = fixture;

    [Fact]
    public async Task application_framework_selection_survives_restore_compile_and_bundle()
    {
        var root = Environment.GetEnvironmentVariable("ISOPLANE_BUILD_ROOT")
            ?? throw new InvalidOperationException("Run this suite with task test:managed.");
        var sdk = Path.Combine(root, "guest-sdks", "dotnet");
        foreach (var version in new[] { "10.0.11", "10.0.12" })
        {
            using var directory = new BundleDirectory();
            var project = new System.Xml.Linq.XElement("Project", new System.Xml.Linq.XAttribute("Sdk", "Microsoft.NET.Sdk.Web"),
                new System.Xml.Linq.XElement("Import", new System.Xml.Linq.XAttribute("Project", Path.Combine(sdk, "AspNetCore/buildTransitive/Isoplane.AspNetCore.props"))),
                new System.Xml.Linq.XElement("PropertyGroup",
                    new System.Xml.Linq.XElement("TargetFramework", "net10.0"),
                    new System.Xml.Linq.XElement("RuntimeFrameworkVersion", version),
                    new System.Xml.Linq.XElement("IsoplaneManagedBundleBuild", "true"),
                    new System.Xml.Linq.XElement("OutputType", "Library"),
                    new System.Xml.Linq.XElement("CopyLocalLockFileAssemblies", "true")),
                new System.Xml.Linq.XElement("ItemGroup", new System.Xml.Linq.XElement("PackageReference",
                    new System.Xml.Linq.XAttribute("Include", "Microsoft.Extensions.Logging.Abstractions"),
                    new System.Xml.Linq.XAttribute("Version", "10.0.11"))),
                new System.Xml.Linq.XElement("Import", new System.Xml.Linq.XAttribute("Project", Path.Combine(sdk, "AspNetCore/buildTransitive/Isoplane.AspNetCore.targets"))),
                new System.Xml.Linq.XElement("Import", new System.Xml.Linq.XAttribute("Project", Path.Combine(sdk, "Build/Isoplane.ManagedBundle.targets"))));
            project.Save(Path.Combine(directory.Path, "Application.csproj"));
            await File.WriteAllTextAsync(Path.Combine(directory.Path, "Application.cs"), """
                using Microsoft.AspNetCore.Builder;
                public static class Application {
                    public static WebApplication Create() {
                        var app = WebApplication.CreateBuilder().Build();
                        app.MapGet("/", () => "hello");
                        return app;
                    }
                }
                """);
            var start = new ProcessStartInfo("dotnet") { WorkingDirectory = directory.Path, RedirectStandardOutput = true, RedirectStandardError = true };
            start.Environment["DOTNET_RUNTIME_ID"] = "linux-x64";
            foreach (var argument in new[] { "msbuild", "Application.csproj", "-restore", "-t:IsoplaneManagedBundle",
                "-getProperty:IsoplaneAspNetCoreRuntimeVersion", "-getItem:Analyzer,IsoplaneBundleAssembly,IsoplaneBundlePlatformReference", "-nologo", "-p:NuGetAudit=false" })
                start.ArgumentList.Add(argument);
            using var process = Process.Start(start) ?? throw new InvalidOperationException("Could not run MSBuild.");
            var output = process.StandardOutput.ReadToEndAsync();
            var errors = process.StandardError.ReadToEndAsync();
            using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(180));
            try { await process.WaitForExitAsync(deadline.Token); }
            finally
            {
                if (!process.HasExited) { process.Kill(entireProcessTree: true); await process.WaitForExitAsync(); }
            }
            Assert.True(process.ExitCode == 0, await output + await errors);
            var text = await output;
            using var result = JsonDocument.Parse(text[text.IndexOf('{')..]);
            Assert.Equal(version, result.RootElement.GetProperty("Properties").GetProperty("IsoplaneAspNetCoreRuntimeVersion").GetString());
            var items = result.RootElement.GetProperty("Items");
            string[] Paths(string name) => items.GetProperty(name).EnumerateArray().Select(item => item.GetProperty("Identity").GetString()!.Replace('\\', '/')).ToArray();
            var generator = Assert.Single(Paths("Analyzer"), path => path.EndsWith("Microsoft.AspNetCore.Http.RequestDelegateGenerator.dll", StringComparison.Ordinal));
            Assert.Contains($"/microsoft.aspnetcore.app.ref/{version}/", generator);
            var implementation = Assert.Single(Paths("IsoplaneBundleAssembly"), path => path.EndsWith("/Microsoft.AspNetCore.dll", StringComparison.Ordinal));
            Assert.Contains($"/microsoft.aspnetcore.app.runtime.linux-x64/{version}/", implementation);
            var logging = Assert.Single(Paths("IsoplaneBundleAssembly"), path => path.EndsWith("/Microsoft.Extensions.Logging.Abstractions.dll", StringComparison.Ordinal));
            Assert.Contains("/microsoft.extensions.logging.abstractions/10.0.11/", logging);
            Assert.DoesNotContain(Paths("IsoplaneBundlePlatformReference"), path => path.EndsWith("/Microsoft.AspNetCore.dll", StringComparison.Ordinal));
        }
    }

    [Fact]
    public async Task building_a_bundle_uses_and_cleans_project_local_metadata()
    {
        using var directory = new BundleDirectory();
        var project = directory.Path;
        await File.WriteAllTextAsync(Path.Combine(project, "Application.csproj"), $"""
            <Project Sdk="Microsoft.NET.Sdk">
              <PropertyGroup>
                <TargetFramework>net10.0</TargetFramework>
                <IsoplaneRuntimeDirectory>{System.Security.SecurityElement.Escape(fixture.SharedRuntime)}</IsoplaneRuntimeDirectory>
              </PropertyGroup>
              <Import Project="$(ISOPLANE_BUILD_ROOT)/guest-sdks/dotnet/Build/Isoplane.ManagedBundle.targets" />
            </Project>
            """);
        await File.WriteAllTextAsync(Path.Combine(project, "Code.cs"), """
            using System.Threading.Tasks;
            namespace Isoplane
            {
                // Stand-ins for the SDK types the bundler recognises by name.
                public sealed class HttpHandlerAttribute : System.Attribute { }
                public sealed class Request { }
                public sealed class Response { }
            }
            public static class Application
            {
                [Isoplane.HttpHandler]
                public static Task<Isoplane.Response> HandleAsync(Isoplane.Request request) => Task.FromResult(new Isoplane.Response());
            }
            """);
        await File.WriteAllTextAsync(Path.Combine(project, "NuGet.Config"),
            "<configuration><packageSources><clear /></packageSources></configuration>");
        var output = Path.Combine(project, "application.tar");
        using var deadline = new CancellationTokenSource(TimeSpan.FromMinutes(5));
        await ManagedBundle.CreateAsync(new(project, "Debug", output), deadline.Token);
        var scratch = Path.Combine(project, ".isoplane", "bundle");
        Assert.True(File.Exists(output), "The project build must produce its application bundle.");
        Assert.True(Directory.Exists(scratch) && !Directory.EnumerateFileSystemEntries(scratch).Any(),
            "Bundle metadata must be staged under the project and removed after publication.");
    }

    [Fact]
    public async Task a_bundle_preserves_application_dependencies_symbols_and_runtime_identity()
    {
        using var directory = new BundleDirectory();
        var output = Path.Combine(directory.Path, "application.tar");
        await ManagedBundle.WriteAsync(fixture.Metadata(), output, CancellationToken.None);
        using var stream = File.OpenRead(output);
        using var archive = new TarReader(stream);
        var entries = new Dictionary<string, byte[]>();
        while (archive.GetNextEntry() is { } entry)
        {
            using var bytes = new MemoryStream();
            entry.DataStream!.CopyTo(bytes);
            entries.Add(entry.Name, bytes.ToArray());
        }
        Assert.True(entries.Keys.Order().SequenceEqual(new[] { "application.json", "runtime.json", "assemblies/Application.dll", "assemblies/Application.pdb", "assemblies/Dependency.dll", "assemblies/Dependency.pdb" }.Order()),
            "Only application IL and portable symbols, and for interpreted execution the SDK runtime's identity, should accompany the manifest; sources, scripts and framework implementations must stay out.");
        Assert.True(entries["assemblies/Application.dll"].SequenceEqual(File.ReadAllBytes(fixture.Application)), "The compiled application and its embedded resources must remain unchanged.");
        Assert.True(entries["assemblies/Dependency.dll"].SequenceEqual(File.ReadAllBytes(fixture.Dependency)), "Transitive application dependencies must preserve their original identity and bytes.");
        using var manifest = JsonDocument.Parse(entries["application.json"]);
        Assert.True(manifest.RootElement.GetProperty("Execution").GetString() == "Interpreted", "An application package must run on the shared interpreter.");
    }

    [Fact]
    public async Task satellites_with_the_same_name_preserve_their_culture_directories()
    {
        using var directory = new BundleDirectory();
        var output = Path.Combine(directory.Path, "application.tar");
        await ManagedBundle.WriteAsync(fixture.Metadata(includeSatellites: true), output, default);
        using var input = File.OpenRead(output);
        using var reader = new TarReader(input);
        var satellites = new Dictionary<string, byte[]>();
        while (reader.GetNextEntry() is { } entry)
            if (entry.Name.EndsWith(".resources.dll", StringComparison.Ordinal))
            {
                using var bytes = new MemoryStream();
                entry.DataStream!.CopyTo(bytes);
                satellites.Add(entry.Name, bytes.ToArray());
            }
        Assert.True(satellites.Count == 2 && satellites["assemblies/fr/Application.resources.dll"].SequenceEqual(File.ReadAllBytes(fixture.Satellite("fr"))) &&
            satellites["assemblies/fr-CA/Application.resources.dll"].SequenceEqual(File.ReadAllBytes(fixture.Satellite("fr-CA"))),
            "ResourceManager satellite assemblies must keep their culture identity and bytes instead of colliding by simple name.");
    }

    [Fact]
    public async Task the_sdk_discovers_assets_and_satellites_for_the_bundle_reader()
    {
        using var directory = new BundleDirectory();
        var output = Path.Combine(directory.Path, "sdk.tar");
        await ManagedBundle.WriteAsync(fixture.SdkMetadata, output, default);
        using var input = File.OpenRead(output);
        using var reader = new TarReader(input);
        var names = new HashSet<string>();
        while (reader.GetNextEntry() is { } entry) names.Add(entry.Name);
        Assert.True(names.Contains("assets/data/greeting.txt") && names.Contains("assemblies/fr/Application.resources.dll") &&
            names.Contains("assemblies/fr-CA/Application.resources.dll"),
            "The real SDK target must discover copied assets and generated satellites, preserving their output paths for bundling.");
    }

    [Fact]
    public async Task a_missing_application_dependency_is_reported_before_archive_creation()
    {
        using var directory = new BundleDirectory();
        var output = Path.Combine(directory.Path, "application.tar");
        var error = await Assert.ThrowsAsync<AssemblyInputException>(() => ManagedBundle.WriteAsync(fixture.Metadata(includeDependency: false), output, CancellationToken.None));
        Assert.True(error.Message.Contains("missing dependency 'Dependency'", StringComparison.Ordinal), "The error must identify the missing transitive dependency.");
        Assert.True(!File.Exists(output), "Invalid dependency closures must not produce an uploadable archive.");
    }

    [Theory]
    [InlineData("config/data.json")]
    [InlineData("config\\data.json")]
    public async Task external_assets_preserve_their_output_paths_and_contents(string target)
    {
        using var directory = new BundleDirectory();
        var asset = Path.Combine(directory.Path, "required-data.json");
        await File.WriteAllTextAsync(asset, "application data");
        var output = Path.Combine(directory.Path, "application.tar");
        await ManagedBundle.WriteAsync(fixture.Metadata(asset: asset, assetTarget: target), output, default);
        using var stream = File.OpenRead(output);
        using var archive = new TarReader(stream);
        var found = false;
        while (archive.GetNextEntry() is { } entry)
            if (entry.Name == "assets/config/data.json")
            {
                using var reader = new StreamReader(entry.DataStream!);
                found = await reader.ReadToEndAsync() == "application data";
            }
        Assert.True(found, "Application assets must be uploaded at their declared output path without changing their bytes.");
    }

    [Theory]
    [InlineData("../escape")]
    [InlineData("/absolute")]
    [InlineData("a\\..\\escape")]
    [InlineData("\\absolute")]
    [InlineData("C:\\absolute")]
    public async Task asset_output_paths_cannot_escape_the_guest_root(string target)
    {
        using var directory = new BundleDirectory();
        var asset = Path.Combine(directory.Path, "data.txt");
        await File.WriteAllTextAsync(asset, "data");
        await Assert.ThrowsAsync<InvalidOperationException>(() => ManagedBundle.WriteAsync(fixture.Metadata(asset: asset, assetTarget: target),
            Path.Combine(directory.Path, "application.tar"), default));
    }

    [Fact]
    public async Task an_interpreted_bundle_carries_its_sdk_runtime_identity_unchanged()
    {
        using var directory = new BundleDirectory();
        var output = Path.Combine(directory.Path, "application.tar");
        await ManagedBundle.WriteAsync(fixture.Metadata(), output, default);
        using var stream = File.OpenRead(output);
        using var archive = new TarReader(stream);
        string? identity = null;
        while (archive.GetNextEntry() is { } entry)
            if (entry.Name == "runtime.json")
            {
                using var reader = new StreamReader(entry.DataStream!);
                identity = await reader.ReadToEndAsync();
            }
        Assert.True(identity == await File.ReadAllTextAsync(Path.Combine(fixture.SharedRuntime, "runtime.json")),
            "The platform runs a package on the runtime its SDK carries, so the bundle must carry that runtime's identity byte for byte.");
    }

    [Fact]
    public async Task an_interpreted_bundle_without_its_sdk_runtime_identity_is_refused()
    {
        using var directory = new BundleDirectory();
        var output = Path.Combine(directory.Path, "application.tar");
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => ManagedBundle.WriteAsync(fixture.Metadata(runtimeDirectory: directory.Path), output, default));
        Assert.True(error.Message.Contains("runtime.json", StringComparison.Ordinal) && !File.Exists(output),
            "A package the platform cannot match to a runtime must be refused before anything is uploaded.");
    }

    [Fact]
    public async Task a_conflicting_application_identity_is_rejected()
    {
        using var directory = new BundleDirectory();
        var duplicate = Path.Combine(directory.Path, "duplicate.dll");
        File.Copy(fixture.Dependency, duplicate);
        var error = await Assert.ThrowsAsync<AssemblyInputException>(() => ManagedBundle.WriteAsync(fixture.Metadata(additionalAssembly: duplicate), Path.Combine(directory.Path, "application.tar"), CancellationToken.None));
        Assert.True(error.Message.Contains("Conflicting inputs define assembly 'Dependency'", StringComparison.Ordinal), "Two files claiming the same dependency identity must not be arbitrarily selected.");
    }

    [Fact]
    public async Task cancellation_preserves_an_existing_bundle_and_cleans_partial_output()
    {
        using var directory = new BundleDirectory();
        var output = Path.Combine(directory.Path, "application.tar");
        await File.WriteAllTextAsync(output, "previous bundle");
        using var cancellation = new CancellationTokenSource();
        cancellation.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => ManagedBundle.WriteAsync(fixture.Metadata(), output, cancellation.Token));
        Assert.True(await File.ReadAllTextAsync(output) == "previous bundle" && Directory.GetFiles(directory.Path).Length == 1,
            "A cancelled build must preserve the previous bundle and leave no partial upload files.");
    }

    [Theory]
    [InlineData("assembly")]
    [InlineData("symbols")]
    public async Task malformed_inputs_produce_actionable_diagnostics_and_no_partial_archive(string input)
    {
        using var directory = new BundleDirectory();
        var application = Path.Combine(directory.Path, "Application.dll");
        File.Copy(fixture.Application, application);
        var invalid = input == "assembly" ? application : Path.ChangeExtension(application, ".pdb");
        await File.WriteAllTextAsync(invalid, "not managed metadata");
        var output = Path.Combine(directory.Path, "application.tar");
        var error = await Assert.ThrowsAnyAsync<InvalidOperationException>(() => ManagedBundle.WriteAsync(fixture.Metadata(applicationPath: application), output, CancellationToken.None));
        Assert.True(error.Message.Contains(input == "assembly" ? "not a valid managed assembly" : "portable PDB", StringComparison.Ordinal),
            "Malformed managed inputs must name the unsupported input format.");
        Assert.True(!File.Exists(output) && Directory.GetFiles(directory.Path, "*.tmp").Length == 0, "Malformed inputs must leave no complete or partial upload archive.");
    }

    [Fact]
    public async Task symbolic_links_are_not_written_as_bundle_inputs()
    {
        using var directory = new FileSystemTestDirectory("isoplane-bundle-links-");
        var application = Path.Combine(directory.Path, "Application.dll");
        File.CreateSymbolicLink(application, fixture.Application);
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => ManagedBundle.WriteAsync(fixture.Metadata(applicationPath: application), Path.Combine(directory.Path, "application.tar"), CancellationToken.None));
        Assert.True(error.Message.Contains("regular file", StringComparison.Ordinal), "A link must not be uploaded as a tar link or followed outside the declared assembly input.");
    }
}

/// <summary>Read-only bundle inputs prepared by Task for this platform.</summary>
public sealed class ManagedBundleFixture
{
    private readonly string[] platform;
    internal JsonElement SdkMetadata { get; }
    internal string Application { get; }
    internal string Dependency { get; }
    internal string Satellite(string culture) => Path.Combine(Path.GetDirectoryName(Application)!, culture, "Application.resources.dll");
    internal string SharedRuntime => SdkMetadata.GetProperty("Properties").GetProperty("IsoplaneRuntimeDirectory").GetString()!;

    /// <summary>Load prepared SDK metadata, reporting the preparation command when it is missing.</summary>
    public ManagedBundleFixture()
    {
        var prepared = Environment.GetEnvironmentVariable("ISOPLANE_TEST_MANAGED_BUNDLE")
            ?? throw new InvalidOperationException("Run task test:managed-bundle-fixture before the managed bundle tests.");
        var metadata = Path.Combine(prepared, "result.json");
        if (!File.Exists(metadata))
            throw new InvalidOperationException($"Missing {metadata}. Run task test:managed-bundle-fixture.");
        using var result = JsonDocument.Parse(File.ReadAllBytes(metadata));
        SdkMetadata = result.RootElement.Clone();
        var items = SdkMetadata.GetProperty("Items");
        var assemblies = items.GetProperty("IsoplaneBundleAssembly").EnumerateArray()
            .Select(item => item.GetProperty("FullPath").GetString()!).ToArray();
        var symbols = items.GetProperty("IsoplaneBundleSymbol").EnumerateArray()
            .Select(item => item.GetProperty("FullPath").GetString()!);
        foreach (var path in assemblies.Concat(symbols).Append(Path.Combine(SharedRuntime, "runtime.json")))
            if (!File.Exists(path))
                throw new InvalidOperationException($"Missing {path}. Run task test:managed-bundle-fixture.");
        Application = assemblies.Single(path => Path.GetFileName(path) == "Application.dll");
        Dependency = assemblies.Single(path => Path.GetFileName(path) == "Dependency.dll");
        platform = items.GetProperty("ReferencePath").EnumerateArray()
            .Select(item => item.GetProperty("FullPath").GetString()!).Where(path => Path.GetFileName(path) != "Dependency.dll").ToArray();
    }

    internal JsonElement Metadata(bool includeDependency = true, string? asset = null, string? additionalAssembly = null, string? applicationPath = null, string? assetTarget = null, bool includeSatellites = false, string? runtimeDirectory = null)
    {
        var assemblies = new List<string> { applicationPath ?? Application };
        if (includeDependency) assemblies.Add(Dependency);
        if (includeSatellites) assemblies.AddRange(new[] { Satellite("fr"), Satellite("fr-CA") });
        if (additionalAssembly is not null) assemblies.Add(additionalAssembly);
        return JsonSerializer.SerializeToElement(new
        {
            Properties = new { AssemblyName = "Application", IsoplaneRuntimeDirectory = runtimeDirectory ?? SharedRuntime },
            Items = new
            {
                IsoplaneBundleAssembly = assemblies.Select(path => new { FullPath = path }),
                IsoplaneBundleSymbol = assemblies.Where(path => !path.EndsWith(".resources.dll", StringComparison.Ordinal)).Select(path => new { FullPath = Path.ChangeExtension(path, ".pdb") }),
                IsoplaneBundlePlatformReference = platform.Select(path => new { FullPath = path }),
                IsoplaneBundleAsset = (asset is null ? Array.Empty<string>() : [asset]).Select(path => new { FullPath = path, TargetPath = assetTarget ?? Path.GetFileName(path) })
            }
        });
    }

}

internal sealed class BundleDirectory : IDisposable
{
    internal string Path { get; } = Directory.CreateTempSubdirectory("isoplane-bundle-test-").FullName;
    public void Dispose() => Directory.Delete(Path, recursive: true);
}
