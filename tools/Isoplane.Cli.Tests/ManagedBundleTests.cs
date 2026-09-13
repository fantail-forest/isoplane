using System.Diagnostics;
using System.Formats.Tar;
using System.Text.Json;
using Xunit;

namespace Wisp.Cli.Tests;

public sealed class ManagedBundleTests : IClassFixture<ManagedBundleFixture>
{
    private readonly ManagedBundleFixture fixture;
    public ManagedBundleTests(ManagedBundleFixture fixture) => this.fixture = fixture;

    [Theory]
    [InlineData("Mono", "Interpreted")]
    [InlineData("NativeAOT", "NativeAot")]
    public async Task a_bundle_preserves_application_dependencies_symbols_and_execution_selection(string profile, string expected)
    {
        using var directory = new BundleDirectory();
        var output = Path.Combine(directory.Path, "application.tar");
        await ManagedBundle.WriteAsync(fixture.Metadata(profile), output, CancellationToken.None);
        using var stream = File.OpenRead(output);
        using var archive = new TarReader(stream);
        var entries = new Dictionary<string, byte[]>();
        while (archive.GetNextEntry() is { } entry)
        {
            using var bytes = new MemoryStream();
            entry.DataStream!.CopyTo(bytes);
            entries.Add(entry.Name, bytes.ToArray());
        }
        Assert.True(entries.Keys.Order().SequenceEqual(new[] { "application.json", "assemblies/Application.dll", "assemblies/Application.pdb", "assemblies/Dependency.dll", "assemblies/Dependency.pdb" }.Order()),
            "Only application IL and portable symbols should accompany the manifest; sources, scripts and framework implementations must stay out.");
        Assert.True(entries["assemblies/Application.dll"].SequenceEqual(File.ReadAllBytes(fixture.Application)), "The compiled application and its embedded resources must remain unchanged.");
        Assert.True(entries["assemblies/Dependency.dll"].SequenceEqual(File.ReadAllBytes(fixture.Dependency)), "Transitive application dependencies must preserve their original identity and bytes.");
        using var manifest = JsonDocument.Parse(entries["application.json"]);
        Assert.True(manifest.RootElement.GetProperty("Execution").GetString() == expected, "The submitted execution selection must match the chosen project configuration.");
    }

    [Fact]
    public async Task a_missing_application_dependency_is_reported_before_archive_creation()
    {
        using var directory = new BundleDirectory();
        var output = Path.Combine(directory.Path, "application.tar");
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => ManagedBundle.WriteAsync(fixture.Metadata("Mono", includeDependency: false), output, CancellationToken.None));
        Assert.True(error.Message.Contains("missing dependency 'Dependency'", StringComparison.Ordinal), "The error must identify the missing transitive dependency.");
        Assert.True(!File.Exists(output), "Invalid dependency closures must not produce an uploadable archive.");
    }

    [Fact]
    public async Task external_assets_are_reported_instead_of_silently_omitted()
    {
        using var directory = new BundleDirectory();
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => ManagedBundle.WriteAsync(fixture.Metadata("Mono", asset: "required-data.json"), Path.Combine(directory.Path, "application.tar"), CancellationToken.None));
        Assert.True(error.Message.Contains("required-data.json", StringComparison.Ordinal), "The diagnostic must identify the asset that the deployment cannot yet carry.");
    }

    [Fact]
    public async Task a_conflicting_application_identity_is_rejected()
    {
        using var directory = new BundleDirectory();
        var duplicate = Path.Combine(directory.Path, "duplicate.dll");
        File.Copy(fixture.Dependency, duplicate);
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => ManagedBundle.WriteAsync(fixture.Metadata("Mono", additionalAssembly: duplicate), Path.Combine(directory.Path, "application.tar"), CancellationToken.None));
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
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => ManagedBundle.WriteAsync(fixture.Metadata("Mono"), output, cancellation.Token));
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
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => ManagedBundle.WriteAsync(fixture.Metadata("Mono", applicationPath: application), output, CancellationToken.None));
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
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => ManagedBundle.WriteAsync(fixture.Metadata("Mono", applicationPath: application), Path.Combine(directory.Path, "application.tar"), CancellationToken.None));
        Assert.True(error.Message.Contains("regular file", StringComparison.Ordinal), "A link must not be uploaded as a tar link or followed outside the declared assembly input.");
    }
}

public sealed class ManagedBundleFixture : IAsyncLifetime
{
    private readonly BundleDirectory directory = new();
    private string[] platform = [];
    public string Application => Path.Combine(directory.Path, "Application/bin/Debug/net10.0/Application.dll");
    public string Dependency => Path.Combine(directory.Path, "Dependency/bin/Debug/net10.0/Dependency.dll");

    public async Task InitializeAsync()
    {
        var dependency = Directory.CreateDirectory(Path.Combine(directory.Path, "Dependency"));
        var application = Directory.CreateDirectory(Path.Combine(directory.Path, "Application"));
        await File.WriteAllTextAsync(Path.Combine(directory.Path, "NuGet.Config"), "<configuration><packageSources><clear/></packageSources></configuration>");
        await File.WriteAllTextAsync(Path.Combine(dependency.FullName, "Dependency.csproj"), "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup></Project>");
        await File.WriteAllTextAsync(Path.Combine(dependency.FullName, "Code.cs"), "public static class Dependency { public static string Value => \"dependency\"; }");
        await File.WriteAllTextAsync(Path.Combine(application.FullName, "Application.csproj"), "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup><ItemGroup><ProjectReference Include=\"../Dependency/Dependency.csproj\"/><EmbeddedResource Include=\"data.txt\"/></ItemGroup></Project>");
        await File.WriteAllTextAsync(Path.Combine(application.FullName, "Code.cs"), "public static class Application { public static string Create() => Dependency.Value; }");
        await File.WriteAllTextAsync(Path.Combine(application.FullName, "data.txt"), "embedded data");
        var resultPath = Path.Combine(directory.Path, "result.json");
        var start = new ProcessStartInfo("dotnet") { UseShellExecute = false, RedirectStandardOutput = true, RedirectStandardError = true, WorkingDirectory = directory.Path };
        foreach (var argument in new[] { "msbuild", Path.Combine(application.FullName, "Application.csproj"), "-restore", "-target:Build", "-getItem:ReferencePath", "-getResultOutputFile:" + resultPath, "-nodeReuse:false" }) start.ArgumentList.Add(argument);
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
        platform = result.RootElement.GetProperty("Items").GetProperty("ReferencePath").EnumerateArray()
            .Select(item => item.GetProperty("FullPath").GetString()!).Where(path => Path.GetFileName(path) != "Dependency.dll").ToArray();
    }

    public JsonElement Metadata(string runtime, bool includeDependency = true, string? asset = null, string? additionalAssembly = null, string? applicationPath = null)
    {
        var assemblies = new List<string> { applicationPath ?? Application };
        if (includeDependency) assemblies.Add(Dependency);
        if (additionalAssembly is not null) assemblies.Add(additionalAssembly);
        return JsonSerializer.SerializeToElement(new
        {
            Properties = new { AssemblyName = "Application", WispApplicationFactory = "Application.Create", WispGuestRuntime = runtime },
            Items = new
            {
                WispBundleAssembly = assemblies.Select(path => new { FullPath = path }),
                WispBundleSymbol = assemblies.Select(path => new { FullPath = Path.ChangeExtension(path, ".pdb") }),
                WispBundlePlatformReference = platform.Select(path => new { FullPath = path }),
                WispBundleAsset = (asset is null ? Array.Empty<string>() : [asset]).Select(path => new { FullPath = Path.Combine(directory.Path, path) })
            }
        });
    }

    public Task DisposeAsync() { directory.Dispose(); return Task.CompletedTask; }
}

internal sealed class BundleDirectory : IDisposable
{
    internal string Path { get; } = Directory.CreateTempSubdirectory("wisp-bundle-test-").FullName;
    public void Dispose() => Directory.Delete(Path, recursive: true);
}
