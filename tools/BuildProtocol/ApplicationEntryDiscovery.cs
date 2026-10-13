using System.Collections.Immutable;
using System.Reflection;
using System.Reflection.Metadata;
using System.Reflection.Metadata.Ecma335;
using System.Reflection.PortableExecutable;

namespace Isoplane.Build;

/// <summary>An application assembly that does not declare one usable entry.</summary>
internal sealed class ApplicationEntryException(string message) : InvalidOperationException(message)
{
    internal const string Code = "ISOPLANE1005";
}

/// <summary>
/// Finds what the runtime invokes in an application assembly, from metadata
/// alone: an executable's asynchronous entry point, or a library's attributed
/// static methods.
/// </summary>
internal static class ApplicationEntryDiscovery
{
    private const string HttpHandler = "Isoplane.HttpHandlerAttribute";
    private const string Initializer = "Isoplane.InitializerAttribute";
    private const string Activated = "Isoplane.ActivatedAttribute";
    private const string Task = "System.Threading.Tasks.Task";
    private const string ResponseTask = "System.Threading.Tasks.Task`1<Isoplane.Response>";

    internal static ApplicationEntry Discover(string path)
    {
        using var input = File.OpenRead(path);
        using var pe = new PEReader(input);
        if (!pe.HasMetadata || pe.PEHeaders.CorHeader is not { } header)
            throw new ApplicationEntryException($"'{path}' is not a managed assembly.");
        var metadata = pe.GetMetadataReader();
        var name = metadata.GetString(metadata.GetAssemblyDefinition().Name);
        var attributed = Attributed(metadata);
        var main = header.Flags.HasFlag(CorFlags.NativeEntryPoint) || header.EntryPointTokenOrRelativeVirtualAddress == 0
            ? default : MetadataTokens.EntityHandle(header.EntryPointTokenOrRelativeVirtualAddress);
        if (main.Kind != HandleKind.MethodDefinition)
            return Handlers(metadata, name, attributed);
        if (attributed.Count != 0)
            throw new ApplicationEntryException($"'{name}' has an entry point, so its {Describe(attributed[0].Attribute)} method " +
                $"'{Reference(metadata, attributed[0].Method)}' would never be called. Remove the attribute, or subscribe to " +
                "Isoplane.Initialization.Initializing and Isoplane.Activation.Activated before awaiting RunAsync.");
        return new EntryPointApplication(AsynchronousMain(metadata, (MethodDefinitionHandle)main));
    }

    /// <summary>
    /// The compiler makes a synchronous wrapper the entry point of an
    /// asynchronous <c>Main</c>; the runtime needs the method the wrapper blocks on.
    /// </summary>
    private static MethodReference AsynchronousMain(MetadataReader metadata, MethodDefinitionHandle entry)
    {
        var wrapper = metadata.GetMethodDefinition(entry);
        var candidates = metadata.GetString(wrapper.Name) != "<Main>" ? []
            : metadata.GetTypeDefinition(wrapper.GetDeclaringType()).GetMethods().Where(handle =>
            {
                var method = metadata.GetMethodDefinition(handle);
                var returns = Signature(metadata, method).ReturnType;
                return metadata.GetString(method.Name) is "<Main>$" or "Main"
                    && (returns == Task || returns.StartsWith(Task + "`1<", StringComparison.Ordinal));
            }).ToArray();
        if (candidates.Length != 1)
            throw new ApplicationEntryException($"The entry point '{Reference(metadata, entry)}' is synchronous. " +
                "An Isoplane application returns to the platform while it waits for requests, so it cannot block in Main. " +
                "End Program.cs with 'await app.RunAsync();' in place of 'app.Run();'.");
        return Reference(metadata, candidates[0]);
    }

    private static HandlerApplication Handlers(MetadataReader metadata, string name, List<(string Attribute, MethodDefinitionHandle Method)> attributed)
    {
        var http = Single(metadata, attributed, HttpHandler)
            ?? throw new ApplicationEntryException($"'{name}' declares no application entry. Mark one static method with [HttpHandler], " +
                "for example: [HttpHandler] public static Task<Response> HandleAsync(Request request).");
        var signature = Static(metadata, http, HttpHandler);
        if (signature.ReturnType != ResponseTask || signature.ParameterTypes.Length != 1
            || signature.ParameterTypes[0] is not ("Isoplane.Request" or "Isoplane.StreamingRequest"))
            throw new ApplicationEntryException($"The [HttpHandler] method '{Reference(metadata, http)}' must return Task<Response> and take one parameter: " +
                "a Request, or a StreamingRequest to read the body as it arrives.");
        return new(Reference(metadata, http), Lifecycle(metadata, attributed, Initializer), Lifecycle(metadata, attributed, Activated));
    }

    private static MethodReference? Lifecycle(MetadataReader metadata, List<(string Attribute, MethodDefinitionHandle Method)> attributed, string attribute)
    {
        if (Single(metadata, attributed, attribute) is not { } method) return null;
        var signature = Static(metadata, method, attribute);
        if (signature.ReturnType != Task || signature.ParameterTypes.Length != 0)
            throw new ApplicationEntryException($"The {Describe(attribute)} method '{Reference(metadata, method)}' must return Task and take no parameters.");
        return Reference(metadata, method);
    }

    private static MethodDefinitionHandle? Single(MetadataReader metadata, List<(string Attribute, MethodDefinitionHandle Method)> attributed, string attribute)
    {
        var methods = attributed.Where(item => item.Attribute == attribute).Select(item => item.Method).ToArray();
        if (methods.Length > 1)
            throw new ApplicationEntryException($"An application has one {Describe(attribute)} method, but the attribute marks {methods.Length}: " +
                string.Join(", ", methods.Select(method => Reference(metadata, method).ToString()).Order(StringComparer.Ordinal)) +
                ". Keep the attribute on one and call the others from it.");
        return methods.Length == 0 ? null : methods[0];
    }

    private static MethodSignature<string> Static(MetadataReader metadata, MethodDefinitionHandle handle, string attribute)
    {
        var method = metadata.GetMethodDefinition(handle);
        if (!method.Attributes.HasFlag(MethodAttributes.Static) || method.GetGenericParameters().Count != 0
            || metadata.GetTypeDefinition(method.GetDeclaringType()).GetGenericParameters().Count != 0)
            throw new ApplicationEntryException($"The {Describe(attribute)} method '{Reference(metadata, handle)}' must be a static, non-generic method of a non-generic type.");
        var type = metadata.GetTypeDefinition(method.GetDeclaringType());
        if (type.GetMethods().Count(other => metadata.StringComparer.Equals(metadata.GetMethodDefinition(other).Name, metadata.GetString(method.Name))) != 1)
            throw new ApplicationEntryException($"The {Describe(attribute)} method '{Reference(metadata, handle)}' must not be overloaded; the runtime finds it by name.");
        return Signature(metadata, method);
    }

    private static List<(string Attribute, MethodDefinitionHandle Method)> Attributed(MetadataReader metadata)
    {
        var found = new List<(string, MethodDefinitionHandle)>();
        foreach (var handle in metadata.MethodDefinitions)
            foreach (var attribute in metadata.GetMethodDefinition(handle).GetCustomAttributes())
                if (AttributeType(metadata, metadata.GetCustomAttribute(attribute)) is { } type and (HttpHandler or Initializer or Activated))
                    found.Add((type, handle));
        return found;
    }

    private static string? AttributeType(MetadataReader metadata, CustomAttribute attribute) => attribute.Constructor.Kind switch
    {
        HandleKind.MemberReference when metadata.GetMemberReference((MemberReferenceHandle)attribute.Constructor).Parent is { Kind: HandleKind.TypeReference } parent
            => Names.Of(metadata, (TypeReferenceHandle)parent),
        HandleKind.MethodDefinition => Names.Of(metadata, metadata.GetMethodDefinition((MethodDefinitionHandle)attribute.Constructor).GetDeclaringType()),
        _ => null
    };

    private static string Describe(string attribute) => "[" + attribute["Isoplane.".Length..^"Attribute".Length] + "]";

    private static MethodReference Reference(MetadataReader metadata, MethodDefinitionHandle handle)
    {
        var method = metadata.GetMethodDefinition(handle);
        return new(Names.Of(metadata, method.GetDeclaringType()), metadata.GetString(method.Name));
    }

    private static MethodSignature<string> Signature(MetadataReader metadata, MethodDefinition method) =>
        method.DecodeSignature(new Names(), null);

    /// <summary>Type names as reflection spells them, with <c>+</c> between nested types.</summary>
    private sealed class Names : ISignatureTypeProvider<string, object?>
    {
        internal static string Of(MetadataReader metadata, TypeDefinitionHandle handle)
        {
            var type = metadata.GetTypeDefinition(handle);
            var name = metadata.GetString(type.Name);
            if (!type.GetDeclaringType().IsNil) return Of(metadata, type.GetDeclaringType()) + "+" + name;
            return type.Namespace.IsNil ? name : metadata.GetString(type.Namespace) + "." + name;
        }

        internal static string Of(MetadataReader metadata, TypeReferenceHandle handle)
        {
            var type = metadata.GetTypeReference(handle);
            var name = metadata.GetString(type.Name);
            if (type.ResolutionScope.Kind == HandleKind.TypeReference)
                return Of(metadata, (TypeReferenceHandle)type.ResolutionScope) + "+" + name;
            return type.Namespace.IsNil ? name : metadata.GetString(type.Namespace) + "." + name;
        }

        public string GetTypeFromDefinition(MetadataReader reader, TypeDefinitionHandle handle, byte rawTypeKind) => Of(reader, handle);
        public string GetTypeFromReference(MetadataReader reader, TypeReferenceHandle handle, byte rawTypeKind) => Of(reader, handle);
        public string GetTypeFromSpecification(MetadataReader reader, object? genericContext, TypeSpecificationHandle handle, byte rawTypeKind) =>
            reader.GetTypeSpecification(handle).DecodeSignature(this, genericContext);
        public string GetGenericInstantiation(string genericType, ImmutableArray<string> typeArguments) => genericType + "<" + string.Join(",", typeArguments) + ">";
        public string GetPrimitiveType(PrimitiveTypeCode typeCode) => "System." + typeCode;
        public string GetSZArrayType(string elementType) => elementType + "[]";
        public string GetArrayType(string elementType, ArrayShape shape) => elementType + "[" + new string(',', shape.Rank - 1) + "]";
        public string GetByReferenceType(string elementType) => elementType + "&";
        public string GetPointerType(string elementType) => elementType + "*";
        public string GetPinnedType(string elementType) => elementType;
        public string GetModifiedType(string modifier, string unmodifiedType, bool isRequired) => unmodifiedType;
        public string GetGenericMethodParameter(object? genericContext, int index) => "!!" + index;
        public string GetGenericTypeParameter(object? genericContext, int index) => "!" + index;
        public string GetFunctionPointerType(MethodSignature<string> signature) => "fnptr";
    }
}
