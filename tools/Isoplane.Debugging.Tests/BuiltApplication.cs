using System.Formats.Tar;
using System.Text;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.Emit;
using Xunit;

namespace Isoplane.Debugging.Tests;

/// <summary>A source file compiled into an application package, as a local session builds one.</summary>
internal static class BuiltApplication
{
    /// <summary>Compiles <paramref name="source"/> as the file at <paramref name="path"/> and writes the package archive at <paramref name="package"/>.</summary>
    internal static void Write(string package, string path, string source)
    {
        var references = ((string)AppContext.GetData("TRUSTED_PLATFORM_ASSEMBLIES")!).Split(Path.PathSeparator)
            .Select(reference => MetadataReference.CreateFromFile(reference));
        var compilation = CSharpCompilation.Create("Application",
            [CSharpSyntaxTree.ParseText(source, path: path, encoding: Encoding.UTF8)], references,
            new CSharpCompilationOptions(OutputKind.DynamicallyLinkedLibrary, optimizationLevel: OptimizationLevel.Debug));
        using var image = new MemoryStream();
        using var symbols = new MemoryStream();
        var emitted = compilation.Emit(image, symbols, options: new EmitOptions(debugInformationFormat: DebugInformationFormat.PortablePdb));
        Assert.True(emitted.Success, "The application fixture must compile: " + string.Join("; ", emitted.Diagnostics));
        using var archive = File.Create(package);
        using var writer = new TarWriter(archive);
        foreach (var (name, content) in new[] { ("assemblies/Application.dll", image), ("assemblies/Application.pdb", symbols) })
        {
            content.Position = 0;
            writer.WriteEntry(new PaxTarEntry(TarEntryType.RegularFile, name) { DataStream = content });
        }
    }
}
