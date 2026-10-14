using System.Collections.Immutable;
using System.Reflection;
using System.Reflection.Metadata;
using System.Reflection.Metadata.Ecma335;
using System.Reflection.PortableExecutable;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Isoplane.Build;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class AssemblyClosureTests : IDisposable
{
    private readonly string directory = Directory.CreateTempSubdirectory("isoplane-assembly-closure-").FullName;
    private static readonly string CoreLibrary = typeof(object).Assembly.Location;
    private static IReadOnlyDictionary<string, InspectedAssembly> Platform => AssemblyClosure.Platform([AssemblyClosure.InspectFile(CoreLibrary)]);

    [Fact]
    public void ReadyToRun_libraries_with_retained_IL_are_valid_compiler_inputs()
    {
        var path = typeof(System.Text.Json.JsonSerializer).Assembly.Location;
        using var pe = new PEReader(File.OpenRead(path));
        Assert.True(pe.PEHeaders.CorHeader!.ManagedNativeHeaderDirectory.Size > 0,
            "The test must exercise the installed framework's ReadyToRun implementation.");
        var assembly = AssemblyClosure.InspectFile(path);
        var application = AssemblyClosure.Application([assembly]);
        Assert.True(application.ContainsKey(assembly.Identity.Key),
            "A library's retained IL must remain usable when its package also includes ReadyToRun code.");
    }

    [Theory]
    [InlineData("signature")]
    [InlineData("short-header")]
    [InlineData("native-method")]
    [InlineData("missing-il")]
    public void ReadyToRun_inputs_without_a_valid_managed_implementation_are_rejected(string damage)
    {
        var bytes = File.ReadAllBytes(typeof(System.Text.Json.JsonSerializer).Assembly.Location);
        using (var pe = new PEReader(new MemoryStream(bytes)))
        {
            var header = pe.PEHeaders.CorHeader!;
            Assert.True(pe.PEHeaders.TryGetDirectoryOffset(header.ManagedNativeHeaderDirectory, out var nativeOffset),
                "The fixture must provide an addressable ReadyToRun header.");
            if (damage == "signature") bytes[nativeOffset] = 0;
            else if (damage == "short-header")
                System.Buffers.Binary.BinaryPrimitives.WriteInt32LittleEndian(bytes.AsSpan(pe.PEHeaders.CorHeaderStartOffset + 68), 4);
            else
            {
                var metadata = pe.GetMetadataReader();
                var method = metadata.MethodDefinitions.First(handle => metadata.GetMethodDefinition(handle).RelativeVirtualAddress != 0);
                var row = pe.PEHeaders.MetadataStartOffset + metadata.GetTableMetadataOffset(TableIndex.MethodDef)
                    + (MetadataTokens.GetRowNumber(method) - 1) * metadata.GetTableRowSize(TableIndex.MethodDef);
                if (damage == "native-method")
                    System.Buffers.Binary.BinaryPrimitives.WriteUInt16LittleEndian(bytes.AsSpan(row + 4), (ushort)MethodImplAttributes.Native);
                else
                    foreach (var handle in metadata.MethodDefinitions)
                    {
                        var offset = pe.PEHeaders.MetadataStartOffset + metadata.GetTableMetadataOffset(TableIndex.MethodDef)
                            + (MetadataTokens.GetRowNumber(handle) - 1) * metadata.GetTableRowSize(TableIndex.MethodDef);
                        System.Buffers.Binary.BinaryPrimitives.WriteInt32LittleEndian(bytes.AsSpan(offset), 0);
                    }
            }
        }
        using var input = new MemoryStream(bytes);
        var assembly = AssemblyClosure.Inspect(input, "damaged-readytorun.dll");
        var error = Assert.Throws<AssemblyInputException>(() => AssemblyClosure.Application([assembly]));
        Assert.True(error.Problem == AssemblyInputProblem.InvalidAssembly,
            "Native methods, missing IL and unrecognized native headers must remain invalid compiler inputs.");
    }

    [Fact]
    public void Cyclic_nested_type_metadata_is_refused_without_recursing_on_the_host_stack()
    {
        var metadata = new MetadataBuilder();
        metadata.AddModule(0, metadata.GetOrAddString("Malformed.dll"), metadata.GetOrAddGuid(Guid.NewGuid()), default, default);
        metadata.AddAssembly(metadata.GetOrAddString("Malformed"), new Version(1, 0), default, default, 0, AssemblyHashAlgorithm.None);
        metadata.AddTypeDefinition(TypeAttributes.NotPublic, default, metadata.GetOrAddString("<Module>"), default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        var nested = metadata.AddTypeDefinition(TypeAttributes.NestedPrivate, default, metadata.GetOrAddString("Native"), default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        metadata.AddNestedType(nested, nested);
        var method = metadata.AddMethodDefinition(MethodAttributes.Static | MethodAttributes.PinvokeImpl, MethodImplAttributes.PreserveSig,
            metadata.GetOrAddString("Read"), metadata.GetOrAddBlob(new byte[] { 0, 0, 8 }), 0, MetadataTokens.ParameterHandle(1));
        metadata.AddMethodImport(method, MethodImportAttributes.None, metadata.GetOrAddString("read"),
            metadata.AddModuleReference(metadata.GetOrAddString("native")));
        var bytes = new BlobBuilder();
        new ManagedPEBuilder(new PEHeaderBuilder(), new MetadataRootBuilder(metadata), new BlobBuilder()).Serialize(bytes);
        using var input = new MemoryStream(bytes.ToArray());
        var error = Assert.Throws<AssemblyInputException>(() => AssemblyClosure.Inspect(input, "malformed.dll"));
        Assert.True(error.Problem == AssemblyInputProblem.InvalidAssembly,
            "Malformed customer metadata must produce a contained input error instead of exhausting the host stack.");
    }

    [Fact]
    public void A_complete_application_dependency_closure_is_accepted_without_executing_it()
    {
        var dependency = Compile("Dependency", "public static class Dependency { public static object Value() => throw new System.Exception(\"Do not execute\"); }");
        var application = Compile("Customer", "public static class Factory { public static object Create() => Dependency.Value(); }", [dependency]);
        AssemblyClosure.Validate(AssemblyClosure.Application([AssemblyClosure.InspectFile(application), AssemblyClosure.InspectFile(dependency)]), Platform);
        Assert.True(AssemblyClosure.InspectFile(application).References.Any(reference => reference.Name == "Dependency"), "The fixture must require its application dependency rather than accidentally testing a standalone assembly.");
    }

    [Fact]
    public void An_unused_native_declaration_is_diagnosed_only_when_enabled()
    {
        var dependency = Compile("PortableDependency", """
            namespace Portable;
            public static class Library
            {
                public static object Value() => null;
                private static class Native
                {
                    [System.Runtime.InteropServices.DllImport("platform-only", EntryPoint = "native_value")]
                    internal static extern int Read();
                }
            }
            """);
        var application = Compile("Customer", "public static class Factory { public static object Create() => Portable.Library.Value(); }", [dependency]);
        var inputs = AssemblyClosure.Application([AssemblyClosure.InspectFile(application), AssemblyClosure.InspectFile(dependency)]);
        AssemblyClosure.Validate(inputs, Platform);
        Assert.True(!AssemblyClosure.Diagnostics(inputs.Values).Any(), "Native declarations must be silent by default.");
        var diagnostics = AssemblyClosure.Diagnostics(inputs.Values, warnOnNativeImports: true).Select(item => item.ToString()).ToArray();
        Assert.True(diagnostics.Length == 1 && diagnostics[0].Contains("ISOPLANE1001", StringComparison.Ordinal)
            && diagnostics[0].Contains("PortableDependency", StringComparison.Ordinal)
            && diagnostics[0].Contains("Portable.Library.Native.Read", StringComparison.Ordinal)
            && diagnostics[0].Contains("platform-only!native_value", StringComparison.Ordinal),
            "The opt-in diagnostic must identify a transitive native declaration without executing or rejecting the library.");
        Assert.Empty(AssemblyClosure.Diagnostics(inputs.Values, warnOnNativeImports: true, new HashSet<string> { "platform-only!native_value" }));
    }

    [Fact]
    public void Thread_creation_and_synchronous_waits_in_dependencies_have_advisory_diagnostics()
    {
        var dependency = Compile("ThreadedDependency", """
            public static class Work
            {
                public static void Start(System.Threading.Thread thread) => thread.Start();
                public static void Wait(System.Threading.Tasks.Task task) => task.Wait();
            }
            """);
        var assembly = AssemblyClosure.InspectFile(dependency);
        AssemblyClosure.Validate(AssemblyClosure.Application([assembly]), Platform);
        var diagnostics = AssemblyClosure.Diagnostics([assembly]).ToArray();
        Assert.True(diagnostics.Any(item => item.Code == "ISOPLANE1002" && item.Message.Contains("System.Threading.Thread.Start", StringComparison.Ordinal))
            && diagnostics.Any(item => item.Code == "ISOPLANE1003" && item.Message.Contains("System.Threading.Tasks.Task.Wait", StringComparison.Ordinal)),
            "Dependency metadata must identify thread creation and potentially blocking calls without blacklisting the containing assembly.");
    }

    [Fact]
    public void A_missing_transitive_dependency_has_a_specific_diagnostic()
    {
        var dependency = Compile("Dependency", "public static class Dependency { public static object Value() => null; }");
        var application = Compile("Customer", "public static class Factory { public static object Create() => Dependency.Value(); }", [dependency]);
        var error = Assert.Throws<AssemblyInputException>(() => AssemblyClosure.Validate(AssemblyClosure.Application([AssemblyClosure.InspectFile(application)]), Platform));
        Assert.True(error.Problem == AssemblyInputProblem.MissingDependency && error.Message.Contains("Dependency", StringComparison.Ordinal), "The omitted dependency must be identified before compilation.");
    }

    [Fact]
    public void Renaming_a_platform_assembly_file_does_not_allow_runtime_substitution()
    {
        var disguised = Path.Combine(directory, "Customer.dll");
        var implementation = Compile("Platform", "public static class Platform { public static object Value() => null; }");
        File.Copy(implementation, disguised);
        var error = Assert.Throws<AssemblyInputException>(() => AssemblyClosure.Validate(AssemblyClosure.Application([AssemblyClosure.InspectFile(disguised)]), AssemblyClosure.Platform([AssemblyClosure.InspectFile(implementation)])));
        Assert.True(error.Problem == AssemblyInputProblem.PlatformSubstitution, "Platform ownership is determined by metadata identity, not the upload filename.");
    }

    [Fact]
    public void A_matching_name_and_version_with_the_wrong_public_key_is_refused()
    {
        var signed = Compile("Dependency", "public static class Dependency { public static object Value() => null; }", signed: true);
        var application = Compile("Customer", "public static class Factory { public static object Create() => Dependency.Value(); }", [signed]);
        var unsigned = Compile("Dependency", "public static class Dependency { public static object Value() => null; }", subdirectory: "replacement");
        var error = Assert.Throws<AssemblyInputException>(() => AssemblyClosure.Validate(AssemblyClosure.Application([AssemblyClosure.InspectFile(application), AssemblyClosure.InspectFile(unsigned)]), Platform));
        Assert.True(error.Problem == AssemblyInputProblem.IdentityMismatch && error.Message.Contains("PublicKeyToken", StringComparison.Ordinal), "Dependency resolution must not discard the referenced signing identity.");
    }

    [Fact]
    public void Satellite_assemblies_preserve_distinct_cultures_of_the_same_resource_identity()
    {
        var french = Compile("Customer.resources", "[assembly: System.Reflection.AssemblyCulture(\"fr\")]", subdirectory: "fr");
        var german = Compile("Customer.resources", "[assembly: System.Reflection.AssemblyCulture(\"de\")]", subdirectory: "de");
        var assemblies = AssemblyClosure.Application([AssemblyClosure.InspectFile(french), AssemblyClosure.InspectFile(german)]);
        AssemblyClosure.Validate(assemblies, Platform);
        Assert.True(assemblies.ContainsKey("fr/Customer.resources") && assemblies.ContainsKey("de/Customer.resources"),
            "Different satellite cultures must coexist without losing their assembly identity.");
    }

    [Fact]
    public void A_customer_cannot_replace_platform_satellite_resources()
    {
        var platform = Compile("Framework", "public class Framework { }");
        var satellite = Compile("Framework.resources", "[assembly: System.Reflection.AssemblyCulture(\"fr\")]", subdirectory: "fr");
        var error = Assert.Throws<AssemblyInputException>(() => AssemblyClosure.Validate(
            AssemblyClosure.Application([AssemblyClosure.InspectFile(satellite)]), AssemblyClosure.Platform([AssemblyClosure.InspectFile(platform)])));
        Assert.True(error.Problem == AssemblyInputProblem.PlatformSubstitution,
            "A culture-specific resource cannot replace the platform-owned framework's resources.");
    }

    internal string Compile(string name, string source, string[]? references = null, bool signed = false, string subdirectory = "")
    {
        var output = Directory.CreateDirectory(Path.Combine(directory, subdirectory));
        var options = new CSharpCompilationOptions(OutputKind.DynamicallyLinkedLibrary);
        if (signed)
        {
            using var file = File.OpenRead(CoreLibrary);
            using var pe = new PEReader(file);
            var metadata = pe.GetMetadataReader();
            var key = metadata.GetBlobBytes(metadata.GetAssemblyDefinition().PublicKey);
            options = options.WithCryptoPublicKey(ImmutableArray.Create(key)).WithPublicSign(true);
        }
        var compilation = CSharpCompilation.Create(name, [CSharpSyntaxTree.ParseText(source)],
            new[] { CoreLibrary }.Concat(references ?? []).Select(path => MetadataReference.CreateFromFile(path)), options);
        var path = Path.Combine(output.FullName, name + ".dll");
        using var target = File.Create(path);
        var result = compilation.Emit(target);
        Assert.True(result.Success, "The conformance fixture must be real compilable IL: " + string.Join("\n", result.Diagnostics));
        return path;
    }

    public void Dispose() => Directory.Delete(directory, recursive: true);
}
