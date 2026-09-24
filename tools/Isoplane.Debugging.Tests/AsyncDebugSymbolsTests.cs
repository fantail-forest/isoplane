using System.Reflection.Metadata;
using System.Reflection.PortableExecutable;
using System.Text;
using Isoplane.Build;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.Emit;
using Xunit;

namespace Isoplane.Debugging.Tests;

public sealed class AsyncDebugSymbolsTests
{
    [Theory]
    [InlineData(OptimizationLevel.Debug, false)]
    [InlineData(OptimizationLevel.Release, true)]
    public void async_symbols_identify_storage_and_each_suspension(OptimizationLevel optimization, bool valueType)
    {
        const string source = """
            using System.Threading.Tasks;
            public static class Example
            {
                public static async Task<int> Run()
                {
                    await Task.Delay(1);
                    await Task.Delay(2).ConfigureAwait(false);
                    return 42;
                }
            }
            """;
        var references = ((string)AppContext.GetData("TRUSTED_PLATFORM_ASSEMBLIES")!).Split(Path.PathSeparator)
            .Select(path => MetadataReference.CreateFromFile(path));
        var compilation = CSharpCompilation.Create("AsyncExample",
            [CSharpSyntaxTree.ParseText(source, path: "Example.cs", encoding: Encoding.UTF8)], references,
            new CSharpCompilationOptions(OutputKind.DynamicallyLinkedLibrary, optimizationLevel: optimization));
        using var image = new MemoryStream();
        using var pdb = new MemoryStream();
        var emitted = compilation.Emit(image, pdb, options: new EmitOptions(debugInformationFormat: DebugInformationFormat.PortablePdb));
        Assert.True(emitted.Success, "The async symbol fixture must compile: " + string.Join("; ", emitted.Diagnostics));
        image.Position = 0;
        pdb.Position = 0;
        using var pe = new PEReader(image);
        using var symbols = MetadataReaderProvider.FromPortablePdbStream(pdb);
        var assembly = pe.GetMetadataReader();
        var methods = assembly.MethodDefinitions.Select(handle => AsyncDebugSymbols.Read(assembly, symbols.GetMetadataReader(), handle))
            .Where(method => method is not null).ToArray();
        Assert.True(methods.Length == 1, "Only the async continuation must have awaiting-frame symbols.");
        var method = methods[0]!;
        Assert.True(method.IsValueType == valueType, "Boxed optimized state must be distinguished from live reference state.");
        Assert.True(method.Name == "Example.Run", "The logical frame must retain the source method name.");
        Assert.True(method.Awaits.Select(location => location.Line).SequenceEqual([6, 7]),
            "Each suspension must map to its own await statement, including ConfigureAwait(false).");
    }
}
