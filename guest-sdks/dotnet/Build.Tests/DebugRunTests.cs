using System.Xml.Linq;
using Xunit;

public sealed class DebugRunTests : MsbuildTests
{
    [Fact]
    public async Task the_evaluated_run_command_debugs_the_prepared_application_configuration()
    {
        var project = Project();
        var command = Msbuild(project, "-getProperty:RunArguments");
        var result = await Run(command);
        Assert.True(result.Exit == 0, "The run command must evaluate for interpreted debugging: " + result.Output);
        Assert.True(result.Output.Contains("--configuration \"Debug\"", StringComparison.Ordinal),
            "The debugger must preserve the selected project configuration.");
        Assert.True(result.Output.Contains("--debug --no-watch", StringComparison.Ordinal), "The run command must start an owned debug session.");
        Assert.Contains("--no-build", result.Output, StringComparison.Ordinal);
    }

    private string Project() => WriteProject(
        new XElement("PropertyGroup", new XElement("Configuration", "Debug"),
            new XElement("IsoplaneDebug", "true"),
            new XElement("PublishDir", "published/"), new XElement("AssemblyName", "App")),
        new XElement("Import", new XAttribute("Project", Path.Combine(SdkDirectory(), "Build/Isoplane.Run.targets"))));
}
