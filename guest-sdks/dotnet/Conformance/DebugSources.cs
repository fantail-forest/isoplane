using System.Reflection.Metadata;
using System.Reflection.PortableExecutable;
using System.Text.Json;
using Microsoft.Build.Framework;

namespace Isoplane.Conformance;

/// <summary>Records source documents from application and dependency portable symbols for stack filtering.</summary>
public sealed class DebugSources : Microsoft.Build.Utilities.Task
{
    /// <summary>Application and dependency assemblies, excluding platform implementations.</summary>
    [Required] public ITaskItem[] Assemblies { get; set; } = [];

    /// <summary>The application assembly, whose portable symbols are required.</summary>
    [Required] public string Application { get; set; } = "";

    /// <summary>The source index consumed by the local debugger.</summary>
    [Required] public string Output { get; set; } = "";

    /// <summary>SDK-generated runtime entrypoints that are not application source.</summary>
    public ITaskItem[] RuntimeSources { get; set; } = [];

    /// <summary>Authored application compilation inputs; excludes source-generator implementation documents.</summary>
    [Required] public ITaskItem[] ApplicationSources { get; set; } = [];

    /// <summary>Reads portable symbols without loading application code and writes the source index.</summary>
    public override bool Execute()
    {
        try
        {
            var sources = new List<SourceMethod>();
            ReadSymbols(Application, sources);
            var inputs = ApplicationSources.Select(item => item.GetMetadata("FullPath")).ToHashSet(StringComparer.Ordinal);
            sources.RemoveAll(method => !inputs.Contains(method.Source));
            foreach (var assembly in Assemblies)
            {
                var path = assembly.GetMetadata("FullPath");
                if (path == Path.GetFullPath(Application)) continue;
                if (File.Exists(Path.ChangeExtension(path, ".pdb"))) ReadSymbols(path, sources);
            }
            var runtime = RuntimeSources.Select(item => item.GetMetadata("FullPath")).ToHashSet(StringComparer.Ordinal);
            sources.RemoveAll(method => runtime.Contains(method.Source));
            Directory.CreateDirectory(Path.GetDirectoryName(Path.GetFullPath(Output))!);
            File.WriteAllText(Output, JsonSerializer.Serialize(sources.OrderBy(method => method.Source, StringComparer.Ordinal).ThenBy(method => method.StartLine).ThenBy(method => method.Name, StringComparer.Ordinal)));
            return true;
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or BadImageFormatException)
        {
            Log.LogError("Cannot read local debugger source symbols: {0}", error.Message);
            return false;
        }
    }

    private static void ReadSymbols(string path, List<SourceMethod> sources)
    {
        using var stream = File.OpenRead(Path.ChangeExtension(path, ".pdb"));
        using var provider = MetadataReaderProvider.FromPortablePdbStream(stream);
        var metadata = provider.GetMetadataReader();
        using var image = File.OpenRead(path);
        using var pe = new PEReader(image);
        var assembly = pe.GetMetadataReader();
        foreach (var handle in assembly.MethodDefinitions)
        {
            var method = assembly.GetMethodDefinition(handle);
            if (Hidden(assembly, method.GetCustomAttributes()) || HiddenType(assembly, method.GetDeclaringType())) continue;
            var debug = metadata.GetMethodDebugInformation(handle);
            var kickoff = debug.GetStateMachineKickoffMethod();
            var original = kickoff.IsNil ? method : assembly.GetMethodDefinition(kickoff);
            var name = TypeName(assembly, original.GetDeclaringType()) + "." + assembly.GetString(original.Name);
            foreach (var document in debug.GetSequencePoints().Where(point => !point.IsHidden && !point.Document.IsNil).GroupBy(point => point.Document))
                sources.Add(new SourceMethod(metadata.GetString(metadata.GetDocument(document.Key).Name),
                    name, document.Min(point => point.StartLine), document.Max(point => point.EndLine), Isoplane.Build.AsyncDebugSymbols.Read(assembly, metadata, handle)));
        }
    }

    private sealed record SourceMethod(string Source, string Name, int StartLine, int EndLine, Isoplane.Build.AsyncDebugSymbols.Method? Async);

    private static string TypeName(MetadataReader metadata, TypeDefinitionHandle handle)
    {
        var type = metadata.GetTypeDefinition(handle);
        var parent = type.GetDeclaringType();
        var prefix = parent.IsNil ? metadata.GetString(type.Namespace) : TypeName(metadata, parent);
        return (prefix.Length == 0 ? "" : prefix + ".") + metadata.GetString(type.Name);
    }

    private static bool HiddenType(MetadataReader metadata, TypeDefinitionHandle handle)
    {
        if (handle.IsNil) return false;
        var type = metadata.GetTypeDefinition(handle);
        return Hidden(metadata, type.GetCustomAttributes()) || HiddenType(metadata, type.GetDeclaringType());
    }

    private static bool Hidden(MetadataReader metadata, CustomAttributeHandleCollection attributes)
    {
        foreach (var handle in attributes)
        {
            var constructor = metadata.GetCustomAttribute(handle).Constructor;
            if (constructor.Kind != HandleKind.MemberReference) continue;
            var parent = metadata.GetMemberReference((MemberReferenceHandle)constructor).Parent;
            if (parent.Kind != HandleKind.TypeReference) continue;
            var type = metadata.GetTypeReference((TypeReferenceHandle)parent);
            var name = metadata.GetString(type.Namespace) + "." + metadata.GetString(type.Name);
            if (name is "System.CodeDom.Compiler.GeneratedCodeAttribute" or "System.Diagnostics.DebuggerHiddenAttribute" or "System.Diagnostics.DebuggerNonUserCodeAttribute")
                return true;
        }
        return false;
    }
}
