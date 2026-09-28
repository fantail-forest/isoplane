using System.Text.Json;
using System.Xml.Linq;
using Xunit;

public sealed class PackageApplicationTests : MsbuildTests
{
    [Fact]
    public async Task an_application_builds_managed_il_without_a_wasm_toolchain()
    {
        var project = ApplicationProject();
        var command = Msbuild(project, "-t:IsoplaneValidateApplication");
        command.ArgumentList.Add("-getProperty:OutputType,RuntimeIdentifier,IsoplaneManagedBundleBuild");
        var result = await Run(command);
        Assert.True(result.Exit == 0, "An ordinary application must evaluate without native guest tools: " + result.Output);
        using var document = JsonDocument.Parse(result.Output);
        var properties = document.RootElement.GetProperty("Properties");
        Assert.True(properties.GetProperty("OutputType").GetString() == "Library" &&
            properties.GetProperty("RuntimeIdentifier").GetString() == "" &&
            properties.GetProperty("IsoplaneManagedBundleBuild").GetString() == "true",
            "The SDK must select a managed application package without requesting native compilation.");
    }

    private string ApplicationProject(params XElement[] properties) => WriteProject(
        new XElement("Import", new XAttribute("Project", "Sdk.props"), new XAttribute("Sdk", "Microsoft.NET.Sdk")),
        new XElement("PropertyGroup", new XElement("TargetFramework", "net10.0"), properties),
        new XElement("Import", new XAttribute("Project", Path.Combine(SdkDirectory(), "Build/Isoplane.ManagedBundle.props"))),
        new XElement("Import", new XAttribute("Project", "Sdk.targets"), new XAttribute("Sdk", "Microsoft.NET.Sdk")));
}
