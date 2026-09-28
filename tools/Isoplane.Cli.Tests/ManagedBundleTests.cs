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
    public async Task building_a_bundle_uses_and_cleans_project_local_metadata()
    {
        var project = fixture.Project;
        var output = Path.Combine(project, "application.tar");
        await ManagedBundle.CreateAsync(new(project, "Debug", output), CancellationToken.None);
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
        using var directory = new BundleDirectory();
        var application = Path.Combine(directory.Path, "Application.dll");
        File.CreateSymbolicLink(application, fixture.Application);
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => ManagedBundle.WriteAsync(fixture.Metadata(applicationPath: application), Path.Combine(directory.Path, "application.tar"), CancellationToken.None));
        Assert.True(error.Message.Contains("regular file", StringComparison.Ordinal), "A link must not be uploaded as a tar link or followed outside the declared assembly input.");
    }
}

public sealed class ManagedBundleFixture : IAsyncLifetime
{
    private readonly BundleDirectory directory = new();
    private string[] platform = [];
    public JsonElement SdkMetadata { get; private set; }
    public string Project => Path.Combine(directory.Path, "Application");
    public string Application => Path.Combine(directory.Path, "Application/bin/Debug/net10.0/Application.dll");
    public string Satellite(string culture) => Path.Combine(directory.Path, "Application/bin/Debug/net10.0", culture, "Application.resources.dll");
    public string Dependency => Path.Combine(directory.Path, "Dependency/bin/Debug/net10.0/Dependency.dll");

    public async Task InitializeAsync()
    {
        Directory.CreateDirectory(SharedRuntime);
        await File.WriteAllTextAsync(Path.Combine(SharedRuntime, "runtime.json"), "{\"sdkVersion\":\"0.0.0-test\"}\n");
        var dependency = Directory.CreateDirectory(Path.Combine(directory.Path, "Dependency"));
        var application = Directory.CreateDirectory(Path.Combine(directory.Path, "Application"));
        await File.WriteAllTextAsync(Path.Combine(directory.Path, "NuGet.Config"), "<configuration><packageSources><clear/></packageSources></configuration>");
        await File.WriteAllTextAsync(Path.Combine(dependency.FullName, "Dependency.csproj"), "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup></Project>");
        await File.WriteAllTextAsync(Path.Combine(dependency.FullName, "Code.cs"), "public static class Dependency { public static string Value => \"dependency\"; }");
        await File.WriteAllTextAsync(Path.Combine(application.FullName, "Application.csproj"), "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup><ItemGroup><ProjectReference Include=\"../Dependency/Dependency.csproj\"/><EmbeddedResource Include=\"data.txt\"/></ItemGroup></Project>");
        await File.WriteAllTextAsync(Path.Combine(application.FullName, "Code.cs"), "public static class Application { public static string Create() => Dependency.Value; }");
        await File.WriteAllTextAsync(Path.Combine(application.FullName, "data.txt"), "embedded data");
        foreach (var culture in new[] { "fr", "fr-CA" })
            await File.WriteAllTextAsync(Path.Combine(application.FullName, "Messages." + culture + ".resx"),
                "<root><data name=\"Greeting\" xml:space=\"preserve\"><value>" + culture + " greeting</value></data></root>");
        var project = System.Xml.Linq.XDocument.Load(Path.Combine(application.FullName, "Application.csproj"));
        project.Root!.Add(new System.Xml.Linq.XElement("PropertyGroup",
            new System.Xml.Linq.XElement("IsoplaneManagedBundleBuild", "true"),
            new System.Xml.Linq.XElement("IsoplaneApplicationFactory", "Application.Create"),
            new System.Xml.Linq.XElement("IsoplaneRuntimeDirectory", SharedRuntime)));
        project.Root.Add(new System.Xml.Linq.XElement("ItemGroup", new System.Xml.Linq.XElement("None",
            new System.Xml.Linq.XAttribute("Update", "data.txt"), new System.Xml.Linq.XElement("CopyToOutputDirectory", "PreserveNewest"),
            new System.Xml.Linq.XElement("TargetPath", "data/greeting.txt"))));
        project.Root.Add(new System.Xml.Linq.XElement("Import", new System.Xml.Linq.XAttribute("Project", Path.Combine(AppContext.BaseDirectory, "Isoplane.ManagedBundle.targets"))));
        project.Save(Path.Combine(application.FullName, "Application.csproj"));
        var resultPath = Path.Combine(directory.Path, "result.json");
        var start = new ProcessStartInfo("dotnet") { UseShellExecute = false, RedirectStandardOutput = true, RedirectStandardError = true, WorkingDirectory = directory.Path };
        foreach (var argument in new[] { "msbuild", Path.Combine(application.FullName, "Application.csproj"), "-restore", "-target:IsoplaneManagedBundle", "-getProperty:AssemblyName,IsoplaneApplicationFactory,IsoplaneRuntimeDirectory", "-getItem:ReferencePath,IsoplaneBundleAssembly,IsoplaneBundleSymbol,IsoplaneBundleAsset,IsoplaneBundlePlatformReference", "-getResultOutputFile:" + resultPath, "-nodeReuse:false" }) start.ArgumentList.Add(argument);
        using var process = Process.Start(start) ?? throw new InvalidOperationException("Could not build managed bundle fixture.");
        var output = process.StandardOutput.ReadToEndAsync();
        var error = process.StandardError.ReadToEndAsync();
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(90));
        try
        {
            await process.WaitForExitAsync(deadline.Token);
            Assert.True(process.ExitCode == 0, "Managed bundle fixture compilation must succeed: " + await output + await error);
        }
        finally
        {
            if (!process.HasExited) { process.Kill(entireProcessTree: true); await process.WaitForExitAsync(); }
        }
        using var result = JsonDocument.Parse(await File.ReadAllBytesAsync(resultPath));
        SdkMetadata = result.RootElement.Clone();
        platform = result.RootElement.GetProperty("Items").GetProperty("ReferencePath").EnumerateArray()
            .Select(item => item.GetProperty("FullPath").GetString()!).Where(path => Path.GetFileName(path) != "Dependency.dll").ToArray();
    }

    /// <summary>Stands in for the runtimes an SDK carries; a bundle only copies their identity.</summary>
    public string SharedRuntime => Path.Combine(directory.Path, "runtime");

    public JsonElement Metadata(bool includeDependency = true, string? asset = null, string? additionalAssembly = null, string? applicationPath = null, string? assetTarget = null, bool includeSatellites = false, string? runtimeDirectory = null)
    {
        var assemblies = new List<string> { applicationPath ?? Application };
        if (includeDependency) assemblies.Add(Dependency);
        if (includeSatellites) assemblies.AddRange(new[] { Satellite("fr"), Satellite("fr-CA") });
        if (additionalAssembly is not null) assemblies.Add(additionalAssembly);
        return JsonSerializer.SerializeToElement(new
        {
            Properties = new { AssemblyName = "Application", IsoplaneApplicationFactory = "Application.Create", IsoplaneRuntimeDirectory = runtimeDirectory ?? SharedRuntime },
            Items = new
            {
                IsoplaneBundleAssembly = assemblies.Select(path => new { FullPath = path }),
                IsoplaneBundleSymbol = assemblies.Where(path => !path.EndsWith(".resources.dll", StringComparison.Ordinal)).Select(path => new { FullPath = Path.ChangeExtension(path, ".pdb") }),
                IsoplaneBundlePlatformReference = platform.Select(path => new { FullPath = path }),
                IsoplaneBundleAsset = (asset is null ? Array.Empty<string>() : [asset]).Select(path => new { FullPath = Path.Combine(directory.Path, path), TargetPath = assetTarget ?? Path.GetFileName(path) })
            }
        });
    }

    public Task DisposeAsync() { directory.Dispose(); return Task.CompletedTask; }
}

internal sealed class BundleDirectory : IDisposable
{
    internal string Path { get; } = Directory.CreateTempSubdirectory("isoplane-bundle-test-").FullName;
    public void Dispose() => Directory.Delete(Path, recursive: true);
}
