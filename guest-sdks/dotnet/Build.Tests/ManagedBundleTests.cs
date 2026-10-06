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
        var packageId = "Isoplane.Bundle.Test." + Guid.NewGuid().ToString("N");
        new XDocument(new XElement("Project", new XAttribute("Sdk", "Microsoft.NET.Sdk"),
            new XElement("PropertyGroup", new XElement("TargetFramework", "net10.0"), new XElement("PackageId", packageId))))
            .Save(Path.Combine(dependency.FullName, "Dependency.csproj"));
        File.WriteAllText(Path.Combine(dependency.FullName, "Code.cs"), "public static class Dependency { public static string Value => \"value\"; }");
        File.WriteAllText(Path.Combine(directory, "NuGet.Config"), $"""
            <configuration>
              <packageSources>
                <clear/>
                <add key="local" value="packages"/>
                <add key="nuget.org" value="https://api.nuget.org/v3/index.json"/>
              </packageSources>
              <packageSourceMapping>
                <clear/>
                <packageSource key="local"><package pattern="{packageId}"/></packageSource>
                <packageSource key="nuget.org"><package pattern="*"/></packageSource>
              </packageSourceMapping>
            </configuration>
            """);
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
            new XElement("PropertyGroup", new XElement("TargetFramework", "net10.0"), new XElement("IsoplaneManagedBundleBuild", "true")),
            new XElement("ItemGroup", new XElement("Compile", new XAttribute("Remove", "dependency/**/*.cs")),
                packageReference ? new XElement("PackageReference", new XAttribute("Include", packageId), new XAttribute("Version", "1.0.0")) : new XElement("ProjectReference", new XAttribute("Include", "dependency/Dependency.csproj")),
                new XElement("None", new XAttribute("Update", "data.txt"), new XAttribute("CopyToOutputDirectory", "PreserveNewest")),
                new XElement("None", new XAttribute("Update", "optional.txt"), new XAttribute("CopyToOutputDirectory", "PreserveNewest"), new XAttribute("CopyToPublishDirectory", "Never")),
                new XElement("Content", new XAttribute("Include", "NuGet.Config"), new XAttribute("CopyToOutputDirectory", "PreserveNewest"))),
            new XElement("Import", new XAttribute("Project", Path.Combine(SdkDirectory(), "Build/Isoplane.ManagedBundle.props"))),
            new XElement("Import", new XAttribute("Project", "Sdk.targets"), new XAttribute("Sdk", "Microsoft.NET.Sdk")),
            new XElement("Target", new XAttribute("Name", "RuntimeAssetFixture"), new XAttribute("BeforeTargets", "IsoplaneManagedBundle"),
                new XElement("ItemGroup", new XElement("ReferenceCopyLocalPaths", new XAttribute("Include", "runtime.wasm;customer-native.so")),
                    new XElement("RuntimePackAsset", new XAttribute("Include", "runtime.wasm")))),
            new XElement("Import", new XAttribute("Project", Path.Combine(SdkDirectory(), "AspNetCore/buildTransitive/Isoplane.AspNetCore.targets"))),
            new XElement("Import", new XAttribute("Project", Path.Combine(SdkDirectory(), "Build/Isoplane.ManagedBundle.targets")))))
            .Save(project);
        var output = Path.Combine(directory, "inputs.json");
        var start = Msbuild(project, "-target:IsoplaneManagedBundle");
        start.ArgumentList.Add("-restore");
        if (!symbols) start.ArgumentList.Add("-property:DebugType=none");
        start.ArgumentList.Add("-getItem:IsoplaneBundleAssembly,IsoplaneBundleSymbol,IsoplaneBundleAsset,IsoplaneBundlePlatformReference");
        start.ArgumentList.Add("-getResultOutputFile:" + output);
        var result = await Run(start);
        Assert.True(result.Exit == 0, "The managed bundle target must compile and describe an ordinary dependency graph: " + result.Output);
        using var document = JsonDocument.Parse(File.ReadAllBytes(output));
        var items = document.RootElement.GetProperty("Items");
        static string[] Names(JsonElement items, string name) => items.GetProperty(name).EnumerateArray().Select(item => item.GetProperty("Filename").GetString()!).ToArray();
        static bool AspNetImplementation(JsonElement item) => item.GetProperty("FullPath").GetString()!
            .Split(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar)
            .Contains("microsoft.aspnetcore.app.runtime.linux-x64", StringComparer.OrdinalIgnoreCase);
        var bundled = items.GetProperty("IsoplaneBundleAssembly").EnumerateArray().ToLookup(AspNetImplementation, item => item.GetProperty("FullPath").GetString()!);
        Assert.Equal(new[] { "Application", "Dependency" }, bundled[false].Select(Path.GetFileNameWithoutExtension).Order());
        Assert.True(bundled[true].Any(), "The application's selected ASP.NET implementation must ship in the bundle instead of being inherited from the platform.");
        var aspNetPack = Directory.GetFiles(Path.GetDirectoryName(bundled[true].First())!, "*.dll").Select(Path.GetFileName);
        Assert.True(bundled[true].Select(Path.GetFileName).Order().SequenceEqual(aspNetPack.Order()), "The selected ASP.NET implementation must ship in full, at the version the application restored.");
        Assert.True(!Names(items, "IsoplaneBundlePlatformReference").Contains("Dependency"), "A project reference's reference assembly must not be mistaken for a platform dependency.");
        Assert.True(!items.GetProperty("IsoplaneBundlePlatformReference").EnumerateArray().Any(AspNetImplementation), "The selected ASP.NET implementation must not be reported as a platform-supplied reference.");
        var expectedSymbols = !symbols ? Array.Empty<string>() : packageReference ? new[] { "Application" } : new[] { "Application", "Dependency" };
        Assert.True(Names(items, "IsoplaneBundleSymbol").Order().SequenceEqual(expectedSymbols), "Portable symbols must be collected when supplied for application assemblies.");
        Assert.True(Names(items, "IsoplaneBundleAsset").Order().SequenceEqual(new[] { "customer-native", "data" }), "Application data and customer native inputs must be reported; platform runtime artifacts, NuGet settings and explicitly unpublished files must stay local.");
        var applicationPath = items.GetProperty("IsoplaneBundleAssembly").EnumerateArray().Single(item => item.GetProperty("Filename").GetString() == "Application").GetProperty("FullPath").GetString()!;
        using var assembly = File.OpenRead(applicationPath);
        using var pe = new PEReader(assembly);
        var metadata = pe.GetMetadataReader();
        Assert.True(!metadata.TypeDefinitions.Any(handle => metadata.GetString(metadata.GetTypeDefinition(handle).Name) == "IsoplaneGeneratedExports"),
            "A source-free bundle must leave runtime exports to the platform-owned server build.");
    }
}
