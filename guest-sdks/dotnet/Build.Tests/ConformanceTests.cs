using System.Reflection;
using System.Reflection.Metadata;
using System.Reflection.Metadata.Ecma335;
using System.Reflection.PortableExecutable;
using System.Xml.Linq;
using Wisp.Conformance;
using Xunit;

public sealed class ConformanceTests : MsbuildTests
{
    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task local_build_diagnoses_a_transitive_native_declaration_without_rejecting_it(bool malformed)
    {
        var build = Directory.CreateDirectory(Path.Combine(directory, "tools/Build")).FullName;
        var task = Directory.CreateDirectory(Path.Combine(directory, "tools/Conformance")).FullName;
        File.Copy(typeof(InspectManagedAssemblies).Assembly.Location, Path.Combine(task, "Isoplane.Conformance.dll"));
        var targets = Path.Combine(build, "Wisp.Conformance.targets");
        File.Copy(Path.Combine(SdkDirectory(), "Build/Wisp.Conformance.targets"), targets);
        var dependency = Path.Combine(directory, "PortableDependency.dll");
        File.WriteAllBytes(dependency, malformed ? [1, 2, 3] : NativeDeclaration());
        var framework = Path.Combine(directory, "Framework.dll");
        File.WriteAllBytes(framework, NativeDeclaration("Framework"));
        var project = WriteProject(
            new XElement("ItemGroup",
                new XElement("IntermediateAssembly", new XAttribute("Include", typeof(ConformanceTests).Assembly.Location)),
                new XElement("ReferenceCopyLocalPaths", new XAttribute("Include", dependency)),
                new XElement("ReferenceCopyLocalPaths", new XAttribute("Include", framework)),
                new XElement("ReferencePath", new XAttribute("Include", framework), new XElement("FrameworkReferenceName", "Microsoft.NETCore.App"))),
            new XElement("Import", new XAttribute("Project", Path.Combine(SdkDirectory(), "Build/Wisp.ManagedBundle.targets"))),
            new XElement("Import", new XAttribute("Project", targets)),
            new XElement("Target", new XAttribute("Name", "CoreCompile")));
        var result = await Run(Msbuild(project, "-t:CoreCompile"));
        Assert.True(malformed ? result.Exit != 0 && result.Output.Contains("WISP1000", StringComparison.Ordinal)
                : result.Exit == 0 && result.Output.Contains("WISP1001", StringComparison.Ordinal)
                  && result.Output.Contains("Portable.Native.Read", StringComparison.Ordinal),
            "Ordinary compilation must inspect dependencies, warn on declarations and refuse malformed metadata: " + result.Output);
        Assert.True(!result.Output.Contains("Assembly 'Framework'", StringComparison.Ordinal),
            "Platform implementation imports must not drown application compatibility diagnostics in framework warnings.");
    }

    private static byte[] NativeDeclaration(string name = "PortableDependency")
    {
        var metadata = new MetadataBuilder();
        metadata.AddModule(0, metadata.GetOrAddString(name + ".dll"), metadata.GetOrAddGuid(Guid.NewGuid()), default, default);
        metadata.AddAssembly(metadata.GetOrAddString(name), new Version(1, 0), default, default, 0, AssemblyHashAlgorithm.None);
        metadata.AddTypeDefinition(TypeAttributes.NotPublic, default, metadata.GetOrAddString("<Module>"), default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        metadata.AddTypeDefinition(TypeAttributes.Public, metadata.GetOrAddString("Portable"), metadata.GetOrAddString("Native"), default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        var method = metadata.AddMethodDefinition(MethodAttributes.Static | MethodAttributes.PinvokeImpl, MethodImplAttributes.PreserveSig,
            metadata.GetOrAddString("Read"), metadata.GetOrAddBlob(new byte[] { 0, 0, 8 }), 0, MetadataTokens.ParameterHandle(1));
        metadata.AddMethodImport(method, MethodImportAttributes.None, metadata.GetOrAddString("read"),
            metadata.AddModuleReference(metadata.GetOrAddString("platform-only")));
        var bytes = new BlobBuilder();
        new ManagedPEBuilder(new PEHeaderBuilder(), new MetadataRootBuilder(metadata), new BlobBuilder()).Serialize(bytes);
        return bytes.ToArray();
    }
}
