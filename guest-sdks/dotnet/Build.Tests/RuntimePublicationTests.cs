using System.Text.Json;
using System.Xml.Linq;
using Xunit;

public sealed class RuntimePublicationTests : MsbuildTests
{
    [Theory]
    [InlineData("")]
    [InlineData("536870912")]
    public async Task native_validation_uses_only_the_explicit_admission_memory_override(string memoryBytes)
    {
        var module = Path.Combine(directory, "test.wasm");
        File.WriteAllText(module, "fixture");
        var validator = Path.Combine(directory, "validator");
        var log = Path.Combine(directory, "validator.json");
        WriteExecutable(validator, """
            #!/usr/bin/env python3
            import json, os, pathlib, sys
            pathlib.Path(os.environ['ISOPLANE_TEST_VALIDATOR_LOG']).write_text(json.dumps(sys.argv[1:]))
            """);
        var project = WriteProject(new XElement("PropertyGroup", new XElement("NativeBinary", module),
                new XElement("IsoplaneCli", validator), new XElement("IsoplanePlan", "standard"),
                new XElement("IsoplaneMemoryMaxBytes", "536870912"), new XElement("IsoplaneGuestMemoryBytes", memoryBytes)),
            new XElement("Import", new XAttribute("Project", Path.Combine(SdkDirectory(), "Build/Isoplane.Validation.targets"))));
        var start = Msbuild(project, "-t:IsoplaneValidateModule");
        start.Environment["ISOPLANE_TEST_VALIDATOR_LOG"] = log;
        var result = await Run(start);
        Assert.True(result.Exit == 0, "The SDK validation target must run successfully: " + result.Output);
        var arguments = JsonSerializer.Deserialize<string[]>(File.ReadAllText(log))!;
        Assert.True(arguments.Contains("--guest-memory-bytes") == (memoryBytes.Length != 0) && arguments.Contains("standard"),
            "A large linker declaration cannot grant an admission override; an explicit override preserves all other Standard limits.");
        if (memoryBytes.Length != 0) Assert.True(arguments.Contains(memoryBytes), "The admission override must preserve its exact byte value.");
    }

    [Fact]
    public async Task ordinary_publication_cannot_opt_out_of_admission()
    {
        var module = Path.Combine(directory, "test.wasm");
        File.WriteAllText(module, "fixture");
        var project = WriteProject(new XElement("PropertyGroup", new XElement("NativeBinary", module),
                new XElement("IsoplaneCli", Path.Combine(directory, "missing-validator")), new XElement("IsoplaneValidate", "false")),
            new XElement("Import", new XAttribute("Project", Path.Combine(SdkDirectory(), "Build/Isoplane.Validation.targets"))));
        var result = await Run(Msbuild(project, "-t:IsoplaneValidateModule"));
        Assert.True(result.Exit != 0 && result.Output.Contains("ISOPLANE0002", StringComparison.Ordinal), "A missing validator must fail ordinary publication even when the obsolete bypass is supplied: " + result.Output);
    }

    [Theory]
    [InlineData("Debug")]
    [InlineData("Release")]
    public async Task dotnet_run_passes_the_selected_configuration_to_local_metadata_evaluation(string configuration)
    {
        var project = WriteProject(
            new XElement("PropertyGroup", new XElement("Configuration", configuration),
                new XElement("PublishDir", "published/"), new XElement("AssemblyName", "App")),
            new XElement("Import", new XAttribute("Project", Path.Combine(SdkDirectory(), "Build/Isoplane.Run.targets"))));
        var start = Msbuild(project, "-t:ComputeRunArguments");
        start.ArgumentList.Add("-getProperty:RunArguments,RunCommand");
        var result = await Run(start);
        Assert.True(result.Exit == 0, "Run arguments must evaluate successfully: " + result.Output);
        using var document = JsonDocument.Parse(result.Output);
        var properties = document.RootElement.GetProperty("Properties");
        Assert.True(properties.GetProperty("RunArguments").GetString()!.Contains("--configuration \"" + configuration + "\"", StringComparison.Ordinal),
            "The CLI must evaluate UserSecretsId under the same configuration that built the running guest.");
        Assert.True(properties.GetProperty("RunCommand").GetString() == "isoplane",
            "The run command must launch the sandbox CLI even when dotnet watch invokes it.");
    }


}
