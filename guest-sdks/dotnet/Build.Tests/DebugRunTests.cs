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
            "Debugging must build a fresh application package for its owned session.");
    }

    [Fact]
    public async Task dotnet_run_refuses_to_change_nativeaot_into_interpretation_for_debugging()
    {
        var result = await Run(Msbuild(Project("NativeAOT"), "-t:ComputeRunArguments"));
        Assert.True(result.Exit != 0 && result.Output.Contains("explicitly selected interpreted", StringComparison.Ordinal),
            "An unsupported selected mode must fail with an explicit configuration choice: " + result.Output);
    }

    private string Project(string runtime) => WriteProject(
        new XElement("PropertyGroup", new XElement("Configuration", "IsoplaneInterpretedDebug"),
            new XElement("IsoplaneGuestRuntime", runtime), new XElement("IsoplaneDebug", "true"),
            new XElement("PublishDir", "published/"), new XElement("AssemblyName", "App")),
        new XElement("Import", new XAttribute("Project", Path.Combine(SdkDirectory(), "Build/Isoplane.Run.targets"))));
}
