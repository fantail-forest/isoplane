using System.Text.Json;
using System.Xml.Linq;
using Xunit;

public sealed class RuntimePublicationTests : MsbuildTests
{
    [Theory]
    [InlineData("Debug")]
    [InlineData("Release")]
    public async Task the_evaluated_run_command_passes_the_selected_configuration_to_the_cli(string configuration)
    {
        var project = WriteProject(
            new XElement("PropertyGroup", new XElement("Configuration", configuration),
                new XElement("PublishDir", "published/"), new XElement("AssemblyName", "App")),
            new XElement("Import", new XAttribute("Project", Path.Combine(SdkDirectory(), "Build/Isoplane.Run.targets"))));
        // Evaluation alone, with no target run: an editor starts a project this way.
        var start = Msbuild(project, "-getProperty:RunArguments,RunCommand");
        var result = await Run(start);
        Assert.True(result.Exit == 0, "Run arguments must evaluate successfully: " + result.Output);
        using var document = JsonDocument.Parse(result.Output);
        var properties = document.RootElement.GetProperty("Properties");
        var arguments = properties.GetProperty("RunArguments").GetString()!;
        Assert.True(arguments.StartsWith("dev --project ", StringComparison.Ordinal) && arguments.Contains("--configuration \"" + configuration + "\"", StringComparison.Ordinal),
            "The CLI must be told to run the project, under the same configuration that built the running guest: " + arguments);
        Assert.True(properties.GetProperty("RunCommand").GetString() == "isoplane",
            "The run command must launch the sandbox CLI even when dotnet watch invokes it.");
    }


}
