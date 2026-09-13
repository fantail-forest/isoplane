using System.Text.Json;
using System.Xml.Linq;
using System.Reflection.Metadata;
using System.Reflection.PortableExecutable;
using Xunit;

public sealed class ManagedBundleTests : MsbuildTests
{
    [Theory]
    [InlineData(true, true)]
    [InlineData(false, true)]
    [InlineData(true, false)]
    public async Task managed_bundle_target_separates_application_dependencies_from_framework_and_build_inputs(bool symbols, bool packageReference)
    {
        var dependency = Directory.CreateDirectory(Path.Combine(directory, "dependency"));
        var packageId = "Wisp.Bundle.Test." + Guid.NewGuid().ToString("N");
        new XDocument(new XElement("Project", new XAttribute("Sdk", "Microsoft.NET.Sdk"),
            new XElement("PropertyGroup", new XElement("TargetFramework", "net10.0"), new XElement("PackageId", packageId))))
            .Save(Path.Combine(dependency.FullName, "Dependency.csproj"));
        File.WriteAllText(Path.Combine(dependency.FullName, "Code.cs"), "public static class Dependency { public static string Value => \"value\"; }");
        File.WriteAllText(Path.Combine(directory, "NuGet.Config"), "<configuration><packageSources><clear/><add key=\"local\" value=\"packages\"/></packageSources></configuration>");
        Directory.CreateDirectory(Path.Combine(directory, "packages"));
        if (packageReference)
        {
            var pack = Msbuild(Path.Combine(dependency.FullName, "Dependency.csproj"), "-target:Pack");
            pack.ArgumentList.Add("-restore");
            pack.ArgumentList.Add("-property:PackageOutputPath=" + Path.Combine(directory, "packages"));
            var packed = await Run(pack);
            Assert.True(packed.Exit == 0, "The dependency NuGet package must be built from the fixture source: " + packed.Output);
        }
        File.WriteAllText(Path.Combine(directory, "Code.cs"), "public static class Application { public static string Create() => Dependency.Value; }");
        File.WriteAllText(Path.Combine(directory, "data.txt"), "required data");
        File.WriteAllText(Path.Combine(directory, "optional.txt"), "not published");
        File.WriteAllText(Path.Combine(directory, "runtime.wasm"), "platform artifact");
        File.WriteAllText(Path.Combine(directory, "customer-native.so"), "unsupported customer native artifact");
        var project = Path.Combine(directory, "Application.csproj");
        new XDocument(new XElement("Project",
            new XElement("Import", new XAttribute("Project", "Sdk.props"), new XAttribute("Sdk", "Microsoft.NET.Sdk")),
            new XElement("PropertyGroup", new XElement("TargetFramework", "net10.0"), new XElement("WispManagedBundleBuild", "true"),  new XElement("WispApplicationFactory", "Application.Create")),
            new XElement("ItemGroup", new XElement("Compile", new XAttribute("Remove", "dependency/**/*.cs")),
                packageReference ? new XElement("PackageReference", new XAttribute("Include", packageId), new XAttribute("Version", "1.0.0")) : new XElement("ProjectReference", new XAttribute("Include", "dependency/Dependency.csproj")),
                new XElement("None", new XAttribute("Update", "data.txt"), new XAttribute("CopyToOutputDirectory", "PreserveNewest")),
                new XElement("None", new XAttribute("Update", "optional.txt"), new XAttribute("CopyToOutputDirectory", "PreserveNewest"), new XAttribute("CopyToPublishDirectory", "Never")),
                new XElement("Content", new XAttribute("Include", "NuGet.Config"), new XAttribute("CopyToOutputDirectory", "PreserveNewest"))),
            new XElement("Import", new XAttribute("Project", Path.Combine(SdkDirectory(), "Build/Wisp.ManagedBundle.props"))),
            new XElement("Import", new XAttribute("Project", "Sdk.targets"), new XAttribute("Sdk", "Microsoft.NET.Sdk")),
            new XElement("Target", new XAttribute("Name", "RuntimeAssetFixture"), new XAttribute("BeforeTargets", "WispManagedBundle"),
                new XElement("ItemGroup", new XElement("ReferenceCopyLocalPaths", new XAttribute("Include", "runtime.wasm;customer-native.so")),
                    new XElement("RuntimePackAsset", new XAttribute("Include", "runtime.wasm")))),
            new XElement("Import", new XAttribute("Project", Path.Combine(SdkDirectory(), "AspNetCore/buildTransitive/Wisp.AspNetCore.targets"))),
            new XElement("Import", new XAttribute("Project", Path.Combine(SdkDirectory(), "Build/Wisp.ManagedBundle.targets")))))
            .Save(project);
        var output = Path.Combine(directory, "inputs.json");
        var start = Msbuild(project, "-target:WispManagedBundle");
        start.ArgumentList.Add("-restore");
        if (!symbols) start.ArgumentList.Add("-property:DebugType=none");
        start.ArgumentList.Add("-getItem:WispBundleAssembly,WispBundleSymbol,WispBundleAsset,WispBundlePlatformReference");
        start.ArgumentList.Add("-getResultOutputFile:" + output);
        var result = await Run(start);
        Assert.True(result.Exit == 0, "The managed bundle target must compile and describe an ordinary dependency graph: " + result.Output);
        using var document = JsonDocument.Parse(File.ReadAllBytes(output));
        var items = document.RootElement.GetProperty("Items");
        static string[] Names(JsonElement items, string name) => items.GetProperty(name).EnumerateArray().Select(item => item.GetProperty("Filename").GetString()!).ToArray();
        Assert.True(Names(items, "WispBundleAssembly").Order().SequenceEqual(new[] { "Application", "Dependency" }), "The application and NuGet or project-reference implementation must both be bundled.");
        Assert.True(!Names(items, "WispBundlePlatformReference").Contains("Dependency"), "A project reference's reference assembly must not be mistaken for a platform dependency.");
        var expectedSymbols = !symbols ? Array.Empty<string>() : packageReference ? new[] { "Application" } : new[] { "Application", "Dependency" };
        Assert.True(Names(items, "WispBundleSymbol").Order().SequenceEqual(expectedSymbols), "Portable symbols must be collected when supplied for application assemblies.");
        Assert.True(Names(items, "WispBundleAsset").Order().SequenceEqual(new[] { "customer-native", "data" }), "Application data and customer native inputs must be reported; platform runtime artifacts, NuGet settings and explicitly unpublished files must stay local.");
        var applicationPath = items.GetProperty("WispBundleAssembly").EnumerateArray().Single(item => item.GetProperty("Filename").GetString() == "Application").GetProperty("FullPath").GetString()!;
        using var assembly = File.OpenRead(applicationPath);
        using var pe = new PEReader(assembly);
        var metadata = pe.GetMetadataReader();
        Assert.True(!metadata.TypeDefinitions.Any(handle => metadata.GetString(metadata.GetTypeDefinition(handle).Name) == "WispGeneratedExports"),
            "A source-free bundle must leave runtime exports to the platform-owned server build.");
    }
}
