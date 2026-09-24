using System.Xml.Linq;
using Xunit;

public sealed class DebugRunTests : MsbuildTests
{
    [Fact]
    public async Task dotnet_run_debugs_the_explicit_interpreted_configuration()
    {
        var project = Project("Mono");
        var command = Msbuild(project, "-t:ComputeRunArguments");
        command.ArgumentList.Add("-getProperty:RunArguments");
        var result = await Run(command);
        Assert.True(result.Exit == 0, "The standard run target must accept interpreted debugging: " + result.Output);
        Assert.True(result.Output.Contains("--configuration \"IsoplaneInterpretedDebug\"", StringComparison.Ordinal),
            "The debugger must preserve the selected project configuration.");
        Assert.True(result.Output.Contains("--debug --no-watch", StringComparison.Ordinal), "The run target must start an owned debug session.");
        Assert.False(result.Output.Contains("--no-build", StringComparison.Ordinal),
            "Debugging must publish a fresh session-owned module with its allocated endpoint.");
    }

    [Fact]
    public async Task dotnet_run_preserves_nativeaot_for_debugging()
    {
        var command = Msbuild(Project("NativeAOT"), "-t:ComputeRunArguments");
        command.ArgumentList.Add("-getProperty:RunArguments,IsoplaneGuestRuntime");
        var result = await Run(command);
        Assert.True(result.Exit == 0, "The standard run target must accept NativeAOT debugging: " + result.Output);
        using var values = System.Text.Json.JsonDocument.Parse(result.Output);
        var properties = values.RootElement.GetProperty("Properties");
        Assert.True(properties.GetProperty("IsoplaneGuestRuntime").GetString() == "NativeAOT",
            "Debugging must retain NativeAOT execution.");
        Assert.True(properties.GetProperty("RunArguments").GetString()!.Contains("IsoplaneNativeAotDebug", StringComparison.Ordinal),
            "The debugger launch must retain the selected NativeAOT configuration.");
    }

    private string Project(string runtime) => WriteProject(
        new XElement("PropertyGroup", new XElement("Configuration", runtime == "Mono" ? "IsoplaneInterpretedDebug" : "IsoplaneNativeAotDebug"),
            new XElement("IsoplaneGuestRuntime", runtime), new XElement("IsoplaneDebug", "true"),
            new XElement("PublishDir", "published/"), new XElement("AssemblyName", "App")),
        new XElement("Import", new XAttribute("Project", Path.Combine(SdkDirectory(), "Build/Isoplane.Run.targets"))));
}
