using System.Reflection;
using System.Reflection.Metadata;
using System.Reflection.Metadata.Ecma335;
using System.Reflection.PortableExecutable;
using System.Xml.Linq;
using Isoplane.Conformance;
using Xunit;

public sealed class ConformanceTests : MsbuildTests
{
    [Theory]
    [InlineData("application", false, true)]
    [InlineData("project", false, true)]
    [InlineData("package", false, false)]
    [InlineData("package", true, true)]
    public async Task compatibility_warnings_follow_the_selected_assembly_scope(string source, bool dependencies, bool warns)
    {
        var project = DiagnosticProject(source, new XElement("IsoplaneAnalyzeDependencies", dependencies));
        var result = await Run(Msbuild(project, "-t:CoreCompile"));
        Assert.True(result.Exit == 0 && result.Output.Contains("ISOPLANE1002", StringComparison.Ordinal) == warns
            && result.Output.Contains("ISOPLANE1003", StringComparison.Ordinal) == warns,
            "Thread and blocking diagnostics must follow the application/dependency scope: " + result.Output);
        Assert.True(!result.Output.Contains("ISOPLANE1001", StringComparison.Ordinal),
            "Native declarations must not produce warnings by default: " + result.Output);
    }

    [Theory]
    [InlineData("IsoplaneWarnOnNativeImports", "true", true, false)]
    [InlineData("NoWarn", "ISOPLANE1002;ISOPLANE1003", false, false)]
    [InlineData("NoWarn", "ISOPLANE1002,ISOPLANE1003", false, false)]
    [InlineData("WarningsAsErrors", "ISOPLANE1002", false, true)]
    [InlineData("TreatWarningsAsErrors", "true", false, true)]
    public async Task project_warning_controls_apply_to_conformance(string property, string value, bool native, bool fails)
    {
        var result = await Run(Msbuild(DiagnosticProject("application", new XElement(property, value)), "-t:CoreCompile"));
        Assert.True((result.Exit != 0) == fails && result.Output.Contains("ISOPLANE1001", StringComparison.Ordinal) == native,
            "Opt-in native diagnostics and error promotion must follow project settings: " + result.Output);
        if (property == "NoWarn")
            Assert.True(!result.Output.Contains("ISOPLANE1002", StringComparison.Ordinal)
                && !result.Output.Contains("ISOPLANE1003", StringComparison.Ordinal), "NoWarn must suppress selected diagnostics: " + result.Output);
    }

    [Fact]
    public async Task dependency_opt_in_reports_native_imports_without_platform_runtime_noise()
    {
        var project = DiagnosticProject("package", new XElement("IsoplaneAnalyzeDependencies", true),
            new XElement("IsoplaneWarnOnNativeImports", true));
        var result = await Run(Msbuild(project, "-t:CoreCompile"));
        Assert.True(result.Exit == 0 && result.Output.Contains("ISOPLANE1001", StringComparison.Ordinal)
            && result.Output.Contains("PortableDependency", StringComparison.Ordinal)
            && !result.Output.Contains("Assembly 'Framework'", StringComparison.Ordinal),
            "Explicit dependency analysis must inspect package imports while excluding the selected platform runtime: " + result.Output);
    }

    [Fact]
    public async Task suppression_takes_precedence_over_error_promotion()
    {
        var project = DiagnosticProject("application", new XElement("TreatWarningsAsErrors", true),
            new XElement("NoWarn", "ISOPLANE1002"), new XElement("WarningsNotAsErrors", "ISOPLANE1003"));
        var result = await Run(Msbuild(project, "-t:CoreCompile"));
        Assert.True(result.Exit == 0 && !result.Output.Contains("ISOPLANE1002", StringComparison.Ordinal)
            && result.Output.Contains("warning ISOPLANE1003", StringComparison.Ordinal),
            "Suppression and promotion exceptions must work with blanket warnings-as-errors: " + result.Output);
    }

    [Fact]
    public async Task malformed_dependencies_are_rejected_even_when_dependency_warnings_are_disabled()
    {
        var project = DiagnosticProject("package");
        File.WriteAllBytes(Path.Combine(directory, "PortableDependency.dll"), [1, 2, 3]);
        var result = await Run(Msbuild(project, "-t:CoreCompile"));
        Assert.True(result.Exit != 0 && result.Output.Contains("ISOPLANE1000", StringComparison.Ordinal),
            "Disabling advisory diagnostics must not disable metadata validation: " + result.Output);
    }

    [Fact]
    public async Task ordinary_project_references_receive_compatibility_warnings()
    {
        DiagnosticProject("package");
        var library = Directory.CreateDirectory(Path.Combine(directory, "Library")).FullName;
        File.WriteAllText(Path.Combine(library, "Library.csproj"), """
            <Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup></Project>
            """);
        File.WriteAllText(Path.Combine(library, "Work.cs"), "public static class Work { public static void Start(System.Threading.Thread thread) => thread.Start(); }");
        var application = Directory.CreateDirectory(Path.Combine(directory, "Application")).FullName;
        var project = Path.Combine(application, "Application.csproj");
        new XDocument(new XElement("Project", new XAttribute("Sdk", "Microsoft.NET.Sdk"),
            new XElement("PropertyGroup", new XElement("TargetFramework", "net10.0")),
            new XElement("ItemGroup", new XElement("ProjectReference", new XAttribute("Include", Path.Combine(library, "Library.csproj")))),
            new XElement("Import", new XAttribute("Project", Path.Combine(SdkDirectory(), "Build/Isoplane.ManagedBundle.targets"))),
            new XElement("Import", new XAttribute("Project", Path.Combine(directory, "tools/Build/Isoplane.Conformance.targets"))))).Save(project);
        File.WriteAllText(Path.Combine(application, "Application.cs"), """
            namespace Isoplane { class HttpHandlerAttribute : System.Attribute; class Request; class Response; }
            public static class Application
            {
                [Isoplane.HttpHandler] static System.Threading.Tasks.Task<Isoplane.Response> HandleAsync(Isoplane.Request request) => throw null!;
            }
            """);
        var start = Msbuild(project, "-restore");
        start.ArgumentList.Add("-t:Build");
        var result = await Run(start);
        Assert.True(result.Exit == 0 && result.Output.Contains("Assembly 'Library'", StringComparison.Ordinal)
            && result.Output.Contains("ISOPLANE1002", StringComparison.Ordinal),
            "Real project-reference metadata must include customer libraries in default diagnostics: " + result.Output);
    }

    private string DiagnosticProject(string source, params XElement[] properties)
    {
        var build = Directory.CreateDirectory(Path.Combine(directory, "tools/Build")).FullName;
        var task = Directory.CreateDirectory(Path.Combine(directory, "tools/Conformance")).FullName;
        File.Copy(typeof(InspectManagedAssemblies).Assembly.Location, Path.Combine(task, "Isoplane.Conformance.dll"));
        var targets = Path.Combine(build, "Isoplane.Conformance.targets");
        File.Copy(Path.Combine(SdkDirectory(), "Build/Isoplane.Conformance.targets"), targets);
        var dependency = Path.Combine(directory, "PortableDependency.dll");
        File.WriteAllBytes(dependency, NativeDeclaration());
        var item = new XElement(source == "application" ? "IntermediateAssembly" : "ReferenceCopyLocalPaths",
            new XAttribute("Include", dependency));
        if (source == "project") item.Add(new XElement("ReferenceSourceTarget", "ProjectReference"));
        if (source == "package") item.Add(new XElement("NuGetPackageId", "PortableDependency"));
        var framework = Path.Combine(directory, "Framework.dll");
        File.WriteAllBytes(framework, NativeDeclaration("Framework"));
        return WriteProject(new XElement("PropertyGroup", properties), new XElement("ItemGroup", item,
            new XElement("ReferenceCopyLocalPaths", new XAttribute("Include", framework)),
            new XElement("ReferencePath", new XAttribute("Include", framework), new XElement("FrameworkReferenceName", "Microsoft.NETCore.App"))),
            new XElement("Import", new XAttribute("Project", Path.Combine(SdkDirectory(), "Build/Isoplane.ManagedBundle.targets"))),
            new XElement("Import", new XAttribute("Project", targets)),
            new XElement("Target", new XAttribute("Name", "CoreCompile")));
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
        var runtime = metadata.AddAssemblyReference(metadata.GetOrAddString("System.Runtime"), new Version(10, 0), default, default, 0, default);
        AddHandler(metadata, runtime);
        foreach (var (ns, type, member) in new[] { ("System.Threading", "Thread", "Start"), ("System.Threading.Tasks", "Task", "Wait") })
        {
            var reference = metadata.AddTypeReference(runtime, metadata.GetOrAddString(ns), metadata.GetOrAddString(type));
            metadata.AddMemberReference(reference, metadata.GetOrAddString(member), metadata.GetOrAddBlob(new byte[] { 0x20, 0, 1 }));
        }
        var bytes = new BlobBuilder();
        new ManagedPEBuilder(new PEHeaderBuilder(), new MetadataRootBuilder(metadata), new BlobBuilder()).Serialize(bytes);
        return bytes.ToArray();
    }

    /// <summary>The entry an application assembly needs: <c>[HttpHandler] static Task&lt;Response&gt; Handle(Request)</c>.</summary>
    private static void AddHandler(MetadataBuilder metadata, AssemblyReferenceHandle runtime)
    {
        var sdk = metadata.AddAssemblyReference(metadata.GetOrAddString("Isoplane.Sdk"), new Version(1, 0), default, default, 0, default);
        TypeReferenceHandle Type(EntityHandle scope, string ns, string name) =>
            metadata.AddTypeReference(scope, metadata.GetOrAddString(ns), metadata.GetOrAddString(name));
        var signature = new BlobBuilder();
        new BlobEncoder(signature).MethodSignature().Parameters(1,
            returns => returns.Type().GenericInstantiation(Type(runtime, "System.Threading.Tasks", "Task`1"), 1, false)
                .AddArgument().Type(Type(sdk, "Isoplane", "Response"), false),
            parameters => parameters.AddParameter().Type().Type(Type(sdk, "Isoplane", "Request"), false));
        var handler = metadata.AddMethodDefinition(MethodAttributes.Static, MethodImplAttributes.IL,
            metadata.GetOrAddString("Handle"), metadata.GetOrAddBlob(signature), -1, MetadataTokens.ParameterHandle(1));
        var constructor = new BlobBuilder();
        new BlobEncoder(constructor).MethodSignature(isInstanceMethod: true).Parameters(0, returns => returns.Void(), _ => { });
        metadata.AddCustomAttribute(handler, metadata.AddMemberReference(Type(sdk, "Isoplane", "HttpHandlerAttribute"),
            metadata.GetOrAddString(".ctor"), metadata.GetOrAddBlob(constructor)), metadata.GetOrAddBlob(new byte[] { 1, 0, 0, 0 }));
    }
}
