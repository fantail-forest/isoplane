using System.Reflection.Metadata;

namespace Isoplane.Build;

internal static class AsyncDebugSymbols
{
    private static readonly Guid Stepping = new("54FD2AC5-E925-401A-9C2A-F94F171072F8");

    internal sealed record AwaitLocation(string Source, int Line, int Column);
    internal sealed record Method(string StateMachineType, string Name, string NativeState, bool IsValueType, AwaitLocation[] Awaits);

    internal static Method? Read(MetadataReader assembly, MetadataReader symbols, MethodDefinitionHandle handle)
    {
        var debug = symbols.GetMethodDebugInformation(handle);
        var kickoff = debug.GetStateMachineKickoffMethod();
        if (kickoff.IsNil) return null;
        var method = assembly.GetMethodDefinition(handle);
        var original = assembly.GetMethodDefinition(kickoff);
        var points = debug.GetSequencePoints().Where(point => !point.IsHidden).ToArray();
        var awaits = new List<AwaitLocation>();
        foreach (var attribute in symbols.GetCustomDebugInformation(handle))
        {
            var data = symbols.GetCustomDebugInformation(attribute);
            if (symbols.GetGuid(data.Kind) != Stepping) continue;
            var blob = symbols.GetBlobReader(data.Value);
            blob.ReadInt32();
            while (blob.RemainingBytes > 0)
            {
                int offset = blob.ReadInt32();
                blob.ReadInt32();
                blob.ReadCompressedInteger();
                var candidates = points.Where(point => point.Offset <= offset).ToArray();
                if (candidates.Length == 0) throw new BadImageFormatException("An async suspension has no source sequence point.");
                var point = candidates[^1];
                var document = point.Document.IsNil ? debug.Document : point.Document;
                awaits.Add(new AwaitLocation(symbols.GetString(symbols.GetDocument(document).Name), point.StartLine, point.StartColumn));
            }
        }
        var type = method.GetDeclaringType();
        return new Method(TypeName(assembly, type), TypeName(assembly, original.GetDeclaringType()).Replace('+', '.') + "." + assembly.GetString(original.Name),
            string.Concat((assembly.GetString(assembly.GetAssemblyDefinition().Name) + "_" + TypeName(assembly, type))
                .Select(character => char.IsAsciiLetterOrDigit(character) || character == '_' ? character : '_')), IsValueType(assembly, type), awaits.ToArray());
    }

    private static bool IsValueType(MetadataReader metadata, TypeDefinitionHandle handle)
    {
        var parent = metadata.GetTypeDefinition(handle).BaseType;
        if (parent.Kind != HandleKind.TypeReference) return false;
        var type = metadata.GetTypeReference((TypeReferenceHandle)parent);
        return metadata.StringComparer.Equals(type.Namespace, "System") && metadata.StringComparer.Equals(type.Name, "ValueType");
    }

    private static string TypeName(MetadataReader metadata, TypeDefinitionHandle handle)
    {
        var type = metadata.GetTypeDefinition(handle);
        var parent = type.GetDeclaringType();
        if (!parent.IsNil) return TypeName(metadata, parent) + "+" + metadata.GetString(type.Name);
        var space = metadata.GetString(type.Namespace);
        return (space.Length == 0 ? "" : space + ".") + metadata.GetString(type.Name);
    }
}
