using System.Collections.Immutable;
using System.Reflection;
using System.Reflection.Metadata;
using System.Reflection.PortableExecutable;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Wisp.Build;
using Xunit;

namespace Isoplane.Build.Tests;

public sealed class AssemblyClosureTests : IDisposable
{
    private readonly string directory = Directory.CreateTempSubdirectory("wisp-assembly-closure-").FullName;
    private static readonly string CoreLibrary = typeof(object).Assembly.Location;
    private static IReadOnlyDictionary<string, InspectedAssembly> Platform => AssemblyClosure.Platform([AssemblyClosure.InspectFile(CoreLibrary)]);

    [Fact]
    public void A_complete_application_dependency_closure_is_accepted_without_executing_it()
    {
        var dependency = Compile("Dependency", "public static class Dependency { public static object Value() => throw new System.Exception(\"Do not execute\"); }");
        var application = Compile("Customer", "public static class Factory { public static object Create() => Dependency.Value(); }", [dependency]);
        AssemblyClosure.Validate(AssemblyClosure.Application([AssemblyClosure.InspectFile(application), AssemblyClosure.InspectFile(dependency)]), Platform);
        Assert.True(AssemblyClosure.InspectFile(application).References.Any(reference => reference.Name == "Dependency"), "The fixture must require its application dependency rather than accidentally testing a standalone assembly.");
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
