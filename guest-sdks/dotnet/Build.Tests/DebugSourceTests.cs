using System.Runtime.CompilerServices;
using System.Text.Json;
using System.Xml.Linq;
using Isoplane.Conformance;
using Xunit;

public sealed class DebugSourceTests : MsbuildTests
{
    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task stack_sources_come_from_portable_symbols_and_exclude_runtime_entrypoints(bool exclude)
    {
        var output = Path.Combine(directory, "sources.json");
        var project = Project(typeof(DebugSourceTests).Assembly.Location, output, exclude ? SourcePath() : "");
        var result = await Run(Msbuild(project, "-t:Sources"));
        Assert.True(result.Exit == 0, "Source indexing must read real portable symbols: " + result.Output);
        using var index = JsonDocument.Parse(File.ReadAllText(output));
        var sources = index.RootElement.EnumerateArray().Select(method => method.GetProperty("Source").GetString()).ToArray();
        var names = index.RootElement.EnumerateArray().Select(method => method.GetProperty("Name").GetString()).ToArray();
        Assert.True(!names.Contains("DebugSourceTests.HiddenMethod"), "Debugger-hidden methods must be excluded even when they share an application source file.");
        if (!exclude) Assert.True(names.Contains("DebugSourceTests.stack_sources_come_from_portable_symbols_and_exclude_runtime_entrypoints"), "Async methods must retain their original C# names in the source index.");
        if (!exclude)
        {
            var asyncMethod = index.RootElement.EnumerateArray().Single(method => method.GetProperty("Name").GetString()
                == "DebugSourceTests.stack_sources_come_from_portable_symbols_and_exclude_runtime_entrypoints");
            var suspension = asyncMethod.GetProperty("Async").GetProperty("Awaits").EnumerateArray().Single();
            int expectedLine = Array.FindIndex(File.ReadAllLines(SourcePath()), line => line.Contains("var result = await Run", StringComparison.Ordinal)) + 1;
            Assert.True(suspension.GetProperty("Line").GetInt32() == expectedLine,
                "The async source index must locate the suspended await rather than its generated continuation.");
        }
        HiddenMethod();
        Assert.True(sources.Contains(SourcePath()) != exclude,
            "Customer sources must be visible unless identified as SDK-generated runtime entrypoints.");
        Assert.True(sources.Contains(Path.Combine(Path.GetDirectoryName(SourcePath())!, "ConformanceTests.cs")), "Excluding a runtime source must retain other application documents.");
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task source_indexing_rejects_missing_or_invalid_application_symbols(bool malformed)
    {
        var assembly = Path.Combine(directory, "App.dll");
        if (malformed) File.WriteAllText(Path.ChangeExtension(assembly, ".pdb"), "not symbols");
        var output = Path.Combine(directory, "sources.json");
        var result = await Run(Msbuild(Project(assembly, output, ""), "-t:Sources"));
        Assert.True(result.Exit != 0 && result.Output.Contains("Cannot read local debugger source symbols", StringComparison.Ordinal),
            "Unusable application symbols must fail publication with a debugger diagnostic: " + result.Output);
        Assert.False(File.Exists(output), "Rejected symbols must not produce a source index.");
    }

    [System.Diagnostics.DebuggerHidden]
    private static int HiddenMethod() => 42;

    private string Project(string application, string output, string excluded) => WriteProject(
        new XElement("UsingTask", new XAttribute("TaskName", "Isoplane.Conformance.DebugSources"),
            new XAttribute("AssemblyFile", typeof(DebugSources).Assembly.Location)),
        new XElement("Target", new XAttribute("Name", "Sources"),
            new XElement("Isoplane.Conformance.DebugSources", new XAttribute("Application", application),
                new XAttribute("ApplicationSources", SourcePath() + ";" + Path.Combine(Path.GetDirectoryName(SourcePath())!, "ConformanceTests.cs")),
                new XAttribute("Assemblies", application), new XAttribute("RuntimeSources", excluded), new XAttribute("Output", output))));

    private static string SourcePath([CallerFilePath] string path = "") => path;
}
