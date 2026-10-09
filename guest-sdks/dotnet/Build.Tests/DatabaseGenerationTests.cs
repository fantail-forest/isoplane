using System.Diagnostics;
using System.Xml.Linq;
using Xunit;

public sealed class DatabaseGenerationTests : IDisposable
{
    private readonly string directory = Directory.CreateTempSubdirectory("isoplane-database-generation-test-").FullName;

    [Theory]
    [InlineData("Debug")]
    [InlineData("Release")]
    public async Task unchanged_database_builds_reuse_the_generated_model_and_queries(string configuration)
    {
        CopyDesignProject();
        await Build(configuration);
        var generated = GeneratedSources(configuration);
        var timestamps = generated.Select(File.GetLastWriteTimeUtc).ToArray();
        await Build(configuration);
        Assert.True(timestamps.SequenceEqual(generated.Select(File.GetLastWriteTimeUtc)),
            "An unchanged database build must preserve its generated model and query files rather than forcing guest recompilation.");
    }

    [Fact]
    public async Task a_deleted_generated_source_is_recreated_on_the_next_build()
    {
        const string configuration = "Debug";
        CopyDesignProject();
        await Build(configuration);
        var missing = GeneratedSources(configuration).First();
        File.Delete(missing);
        await Build(configuration);
        Assert.True(File.Exists(missing), "A missing generated database source must be regenerated before compiling the guest.");
    }

    [Theory]
    [InlineData("model")]
    [InlineData("query")]
    [InlineData("design")]
    [InlineData("configuration")]
    [InlineData("reference")]
    [InlineData("tool")]
    [InlineData("import")]
    public async Task changed_database_inputs_regenerate_the_model_and_queries(string change)
    {
        const string configuration = "Debug";
        CopyDesignProject();
        var properties = PrepareDependency(change);
        await Build(configuration, properties);
        var manifest = Manifest(configuration);
        var before = File.GetLastWriteTimeUtc(manifest);
        switch (change)
        {
            case "model":
                var model = Path.Combine(directory, "Todo.cs");
                File.WriteAllText(model, File.ReadAllText(model).Replace("public bool Done", "public int Priority { get; set; }\n    public bool Done", StringComparison.Ordinal));
                break;
            case "query":
                var query = Path.Combine(directory, "Todo.cs");
                File.WriteAllText(query, File.ReadAllText(query).Replace("OrderBy(todo", "OrderByDescending(todo", StringComparison.Ordinal));
                break;
            case "design":
                var design = Path.Combine(directory, "Design", "TodoDesignFactory.cs");
                File.WriteAllText(design, File.ReadAllText(design).Replace("Database=todo;", "Database=changed;", StringComparison.Ordinal));
                break;
            case "configuration":
                await Build(configuration, "-p:EFTargetNamespace=TodoApp.UpdatedGenerated");
                Assert.True(File.GetLastWriteTimeUtc(manifest) != before,
                    "Changing database generation options must invalidate previously generated sources even without a design assembly edit.");
                return;
            case "reference":
                File.WriteAllText(Path.Combine(directory, "Dependency", "Version.cs"), "public static class Version { public const int Value = 2; }");
                break;
            case "tool":
                File.WriteAllText(Path.Combine(directory, "tool", "version"), "changed tool");
                break;
            case "import":
                File.WriteAllText(Path.Combine(directory, "Design", "Generation.props"), "<Project><PropertyGroup><GenerationRevision>2</GenerationRevision></PropertyGroup></Project>");
                break;
            default:
                throw new ArgumentOutOfRangeException(nameof(change));
        }
        await Build(configuration, properties);
        Assert.True(File.GetLastWriteTimeUtc(manifest) != before,
            "Changed model, query, or design-time factory code must invalidate database generation.");
    }

    private string[] PrepareDependency(string change)
    {
        var project = Path.Combine(directory, "Design", "Design.csproj");
        var document = XDocument.Load(project);
        switch (change)
        {
            case "reference":
                Directory.CreateDirectory(Path.Combine(directory, "Dependency"));
                File.WriteAllText(Path.Combine(directory, "Dependency", "Dependency.csproj"), "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup></Project>");
                File.WriteAllText(Path.Combine(directory, "Dependency", "Version.cs"), "public static class Version { public const int Value = 1; }");
                document.Root!.Add(new XElement("ItemGroup", new XElement("ProjectReference", new XAttribute("Include", "../Dependency/Dependency.csproj"))));
                document.Save(project);
                break;
            case "tool":
                var tool = Directory.CreateDirectory(Path.Combine(directory, "tool")).FullName;
                File.WriteAllText(Path.Combine(tool, "version"), "initial tool");
                return ["-p:PkgMicrosoft_EntityFrameworkCore_Tasks=" + tool];
            case "import":
                File.WriteAllText(Path.Combine(directory, "Design", "Generation.props"), "<Project><PropertyGroup><GenerationRevision>1</GenerationRevision></PropertyGroup></Project>");
                document.Root!.Add(new XElement("Import", new XAttribute("Project", "Generation.props")));
                document.Save(project);
                break;
        }
        return [];
    }

    private void CopyDesignProject()
    {
        var sample = Path.Combine(MsbuildTests.RepositoryRoot(), "samples", "aspnet-todo");
        Directory.CreateDirectory(Path.Combine(directory, "Design"));
        foreach (var file in new[] { "Todo.cs", "Design/Design.csproj", "Design/TodoDesignFactory.cs" })
            File.Copy(Path.Combine(sample, file), Path.Combine(directory, file));
    }

    private string Manifest(string configuration) => Path.Combine(directory, "Design", "obj", configuration, "net10.0", "Design.EFGeneratedSources.Publish.txt");

    private string[] GeneratedSources(string configuration)
    {
        var files = File.ReadAllLines(Manifest(configuration));
        Assert.True(files.Length > 0 && files.All(File.Exists), "Database generation must record its existing model and query source outputs.");
        return files;
    }

    private async Task Build(string configuration, params string[] properties)
    {
        var start = new ProcessStartInfo("dotnet") { WorkingDirectory = directory, UseShellExecute = false, RedirectStandardOutput = true, RedirectStandardError = true };
        foreach (var argument in new[] { "build", "Design/Design.csproj", "-c", configuration, "--disable-build-servers" }.Concat(properties))
            start.ArgumentList.Add(argument);
        using var process = Process.Start(start) ?? throw new InvalidOperationException("Could not start the database design build.");
        using var deadline = new CancellationTokenSource(TimeSpan.FromMinutes(2));
        var output = process.StandardOutput.ReadToEndAsync();
        var errors = process.StandardError.ReadToEndAsync();
        try
        {
            await process.WaitForExitAsync(deadline.Token);
            Assert.True(process.ExitCode == 0, "The real EF design build must succeed: " + await output + await errors);
        }
        catch (OperationCanceledException failure) when (deadline.IsCancellationRequested)
        {
            if (!process.HasExited)
                process.Kill(entireProcessTree: true);
            await process.WaitForExitAsync();
            throw new TimeoutException($"The {configuration} EF design build exceeded its two-minute deadline in {directory}:\n"
                + await output + await errors, failure);
        }
        finally
        {
            if (!process.HasExited)
            {
                process.Kill(entireProcessTree: true);
                await process.WaitForExitAsync();
            }
        }
    }

    public void Dispose() => Directory.Delete(directory, recursive: true);
}
