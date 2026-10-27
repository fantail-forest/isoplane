using System.Reflection;
using System.Reflection.Metadata;
using System.Reflection.PortableExecutable;

namespace Isoplane.Build;

internal enum AssemblyInputProblem
{
    InvalidAssembly,
    DuplicateIdentity,
    PlatformSubstitution,
    MissingDependency,
    IdentityMismatch
}

internal sealed class AssemblyInputException(AssemblyInputProblem problem, string message) : InvalidOperationException(message)
{
    internal AssemblyInputProblem Problem { get; } = problem;
}

internal sealed record ManagedIdentity(string Name, Version Version, string Culture, string PublicKeyToken)
{
    internal string Key => Culture.Length == 0 ? Name : Culture + "/" + Name;
    internal string RelativePath => Key + ".dll";
    internal static ManagedIdentity From(AssemblyName name) => new(name.Name ?? "", name.Version ?? new Version(0, 0, 0, 0),
        name.CultureName ?? "", Convert.ToHexStringLower(name.GetPublicKeyToken() ?? []));
    public override string ToString() => $"{Name}, Version={Version}, Culture={(Culture.Length == 0 ? "neutral" : Culture)}, PublicKeyToken={(PublicKeyToken.Length == 0 ? "null" : PublicKeyToken)}";
}

internal sealed record NativeImport(string Member, string Library, string EntryPoint);
internal sealed record ReferencedMethod(string Type, string Name);
internal sealed record ConformanceDiagnostic(string Code, string Message)
{
    public override string ToString() => $"warning {Code}: {Message}";
}
internal sealed record InspectedAssembly(string Origin, ManagedIdentity Identity, ManagedIdentity[] References, bool HasManagedIl,
    NativeImport[] NativeImports, ReferencedMethod[] Methods);

internal static class AssemblyClosure
{
    internal static InspectedAssembly InspectFile(string path)
    {
        using var input = File.OpenRead(path);
        return Inspect(input, path);
    }

    internal static InspectedAssembly Inspect(Stream input, string origin)
    {
        try
        {
            using var pe = new PEReader(input, PEStreamOptions.LeaveOpen);
            if (!pe.HasMetadata || pe.PEHeaders.CorHeader is not { } header)
                throw Invalid(origin, "is not a valid managed assembly");
            var metadata = pe.GetMetadataReader();
            if (!metadata.IsAssembly) throw Invalid(origin, "is not an assembly");
            var definition = metadata.GetAssemblyDefinition();
            var name = new AssemblyName
            {
                Name = metadata.GetString(definition.Name), Version = definition.Version,
                CultureName = metadata.GetString(definition.Culture)
            };
            name.SetPublicKey(metadata.GetBlobBytes(definition.PublicKey));
            var identity = ManagedIdentity.From(name);
            if (identity.Name.Length == 0 || identity.Name.IndexOfAny(['/', '\\']) >= 0 || !AssetPaths.IsRelativeFile(identity.RelativePath))
                throw Invalid(origin, "has an invalid assembly name");
            var references = metadata.AssemblyReferences.Select(handle =>
            {
                var reference = metadata.GetAssemblyReference(handle);
                var target = new AssemblyName
                {
                    Name = metadata.GetString(reference.Name), Version = reference.Version,
                    CultureName = metadata.GetString(reference.Culture)
                };
                if (reference.Flags.HasFlag(AssemblyFlags.PublicKey)) target.SetPublicKey(metadata.GetBlobBytes(reference.PublicKeyOrToken));
                else target.SetPublicKeyToken(metadata.GetBlobBytes(reference.PublicKeyOrToken));
                return ManagedIdentity.From(target);
            }).ToArray();
            var imports = metadata.MethodDefinitions.Select(metadata.GetMethodDefinition)
                .Where(method => method.Attributes.HasFlag(MethodAttributes.PinvokeImpl))
                .Select(method =>
                {
                    var import = method.GetImport();
                    return new NativeImport(TypeName(metadata, method.GetDeclaringType()) + "." + metadata.GetString(method.Name),
                        metadata.GetString(metadata.GetModuleReference(import.Module).Name), metadata.GetString(import.Name));
                }).ToArray();
            var methods = metadata.MemberReferences.Select(metadata.GetMemberReference)
                .Where(member => member.GetKind() == MemberReferenceKind.Method)
                .Select(member => ReferencedType(metadata, member.Parent) is { } type ? new ReferencedMethod(type, metadata.GetString(member.Name)) : null)
                .OfType<ReferencedMethod>().Distinct().ToArray();
            return new(origin, identity, references, HasManagedIl(pe, metadata, header), imports, methods);
        }
        catch (BadImageFormatException) { throw Invalid(origin, "is not a valid managed assembly"); }
    }

    private static bool HasManagedIl(PEReader pe, MetadataReader metadata, CorHeader header)
    {
        if (header.Flags.HasFlag(CorFlags.ILOnly)) return true;
        // ReadyToRun packages retain IL beside native code. Only that IL enters the WASM compiler.
        var native = header.ManagedNativeHeaderDirectory;
        if (!header.Flags.HasFlag(CorFlags.ILLibrary) || native.Size < 16) return false;
        var signature = pe.GetSectionData(native.RelativeVirtualAddress).GetReader();
        if (signature.ReadUInt32() != 0x00525452) return false;
        var hasBody = false;
        foreach (var handle in metadata.MethodDefinitions)
        {
            var method = metadata.GetMethodDefinition(handle);
            var code = method.ImplAttributes & MethodImplAttributes.CodeTypeMask;
            if (code is not (MethodImplAttributes.IL or MethodImplAttributes.Runtime)) return false;
            if (method.RelativeVirtualAddress != 0)
            {
                pe.GetMethodBody(method.RelativeVirtualAddress);
                hasBody = true;
            }
        }
        return hasBody;
    }

    private static string TypeName(MetadataReader metadata, TypeDefinitionHandle handle)
    {
        var names = new Stack<string>();
        var visited = new HashSet<TypeDefinitionHandle>();
        while (!handle.IsNil)
        {
            if (!visited.Add(handle)) throw new BadImageFormatException("Cyclic nested type metadata.");
            var type = metadata.GetTypeDefinition(handle);
            names.Push(metadata.GetString(type.Name));
            handle = type.GetDeclaringType();
            if (handle.IsNil && !type.Namespace.IsNil) names.Push(metadata.GetString(type.Namespace));
        }
        return string.Join(".", names.Where(name => name.Length != 0));
    }

    /// <summary>
    /// The metadata name of the type another assembly declares that a member reference's parent names,
    /// as the analyzer spells it: namespace, enclosing types and the type, with a generic type's arity
    /// suffix and none of its arguments. Null for a parent that is not such a type, including the
    /// assembly's own types, which the table of platform members cannot name.
    /// </summary>
    private static string? ReferencedType(MetadataReader metadata, EntityHandle parent)
    {
        if (parent.Kind == HandleKind.TypeSpecification)
        {
            var signature = metadata.GetBlobReader(metadata.GetTypeSpecification((TypeSpecificationHandle)parent).Signature);
            if (signature.ReadSignatureTypeCode() != SignatureTypeCode.GenericTypeInstance
                || signature.ReadSignatureTypeCode() != SignatureTypeCode.TypeHandle) return null;
            parent = signature.ReadTypeHandle();
        }
        if (parent.Kind != HandleKind.TypeReference) return null;
        var names = new Stack<string>();
        var visited = new HashSet<EntityHandle>();
        while (true)
        {
            if (!visited.Add(parent)) throw new BadImageFormatException("Cyclic nested type metadata.");
            var type = metadata.GetTypeReference((TypeReferenceHandle)parent);
            names.Push(metadata.GetString(type.Name));
            if (type.ResolutionScope.Kind != HandleKind.TypeReference)
            {
                if (!type.Namespace.IsNil) names.Push(metadata.GetString(type.Namespace));
                return string.Join(".", names.Where(name => name.Length != 0));
            }
            parent = type.ResolutionScope;
        }
    }

    /// <summary>
    /// Advisory diagnostics for <paramref name="assemblies"/>. Native imports are reported when
    /// <paramref name="warnOnNativeImports"/> is set, except those <paramref name="providedNativeImports"/>
    /// lists as <c>library!entry</c>, which the runtime provides.
    /// </summary>
    internal static IEnumerable<ConformanceDiagnostic> Diagnostics(IEnumerable<InspectedAssembly> assemblies, bool warnOnNativeImports = false,
        IReadOnlySet<string>? providedNativeImports = null)
    {
        foreach (var assembly in assemblies.OrderBy(item => item.Identity.Key, StringComparer.Ordinal))
        {
            var imports = (warnOnNativeImports ? assembly.NativeImports : [])
                .Where(import => providedNativeImports?.Contains(import.Library + "!" + import.EntryPoint) != true);
            foreach (var import in imports.OrderBy(item => item.Member, StringComparer.Ordinal))
                yield return new(BuildDiagnostics.NativeImport.Code, $"Assembly '{assembly.Identity.Name}', member '{import.Member}' declares native import '{import.Library}!{import.EntryPoint}'. " +
                    "The declaration is permitted; an unsupported native call fails if reached. Use managed .NET APIs for platform services. Conformance rules: 1.");
            foreach (var method in assembly.Methods.OrderBy(method => method.Type, StringComparer.Ordinal).ThenBy(method => method.Name, StringComparer.Ordinal))
            {
                if (method is { Type: "System.Threading.Thread", Name: "Start" })
                    yield return new("ISOPLANE1002", $"Assembly '{assembly.Identity.Name}' references '{method.Type}.{method.Name}'. " +
                        "Guest applications cannot start operating-system threads. Use asynchronous .NET APIs for concurrent I/O. Conformance rules: 1.");
                else if (method is { Type: "System.Threading.Thread", Name: "Sleep" or "Join" }
                    or { Type: "System.Threading.Monitor", Name: "Wait" }
                    or { Type: "System.Threading.Tasks.Task", Name: "Wait" or "WaitAll" or "WaitAny" }
                    or { Type: "System.Threading.Tasks.Task`1" or "System.Threading.Tasks.ValueTask`1", Name: "get_Result" }
                    or { Type: "System.Threading.SemaphoreSlim", Name: "Wait" })
                    yield return new("ISOPLANE1003", $"Assembly '{assembly.Identity.Name}' references '{method.Type}.{method.Name}', which can wait synchronously. " +
                        "A guest cannot block waiting for unfinished work; use await and asynchronous wait APIs. Already-completed operations are not rejected by this diagnostic. Conformance rules: 1.");
            }
        }
    }

    internal static IReadOnlyDictionary<string, InspectedAssembly> Application(IEnumerable<InspectedAssembly> inputs)
        => Index(inputs, application: true);

    internal static IReadOnlyDictionary<string, InspectedAssembly> Platform(IEnumerable<InspectedAssembly> inputs)
        => Index(inputs, application: false);

    private static IReadOnlyDictionary<string, InspectedAssembly> Index(IEnumerable<InspectedAssembly> inputs, bool application)
    {
        var assemblies = new Dictionary<string, InspectedAssembly>(StringComparer.OrdinalIgnoreCase);
        foreach (var assembly in inputs)
        {
            if (application && !assembly.HasManagedIl) throw Invalid(assembly.Origin, "must contain portable managed IL");
            if (!assemblies.TryAdd(assembly.Identity.Key, assembly))
                throw new AssemblyInputException(AssemblyInputProblem.DuplicateIdentity,
                    $"Conflicting inputs define assembly '{assembly.Identity.Key}': '{assemblies[assembly.Identity.Key].Origin}' and '{assembly.Origin}'.");
        }
        return assemblies;
    }

    internal static void Validate(IReadOnlyDictionary<string, InspectedAssembly> application,
        IReadOnlyDictionary<string, InspectedAssembly> platform)
    {
        foreach (var assembly in application.Values)
        {
            var neutralName = assembly.Identity.Culture.Length != 0 && assembly.Identity.Name.EndsWith(".resources", StringComparison.OrdinalIgnoreCase)
                ? assembly.Identity.Name[..^".resources".Length] : assembly.Identity.Name;
            if (platform.ContainsKey(assembly.Identity.Key) || platform.ContainsKey(neutralName))
                throw new AssemblyInputException(AssemblyInputProblem.PlatformSubstitution,
                    $"Application input attempts to replace platform assembly '{assembly.Identity.Name}'. Remove it from the bundle; the selected runtime supplies it.");
            foreach (var reference in assembly.References)
            {
                if (!application.TryGetValue(reference.Key, out var resolved) && !platform.TryGetValue(reference.Key, out resolved))
                    throw new AssemblyInputException(AssemblyInputProblem.MissingDependency,
                        $"Assembly '{assembly.Identity.Name}' requires missing dependency '{reference.Name}' {reference.Version}.");
                if (resolved.Identity.Version < reference.Version ||
                    !string.Equals(resolved.Identity.Culture, reference.Culture, StringComparison.OrdinalIgnoreCase) ||
                    !string.Equals(resolved.Identity.PublicKeyToken, reference.PublicKeyToken, StringComparison.Ordinal))
                    throw new AssemblyInputException(AssemblyInputProblem.IdentityMismatch,
                        $"Assembly '{assembly.Identity.Name}' requires '{reference}', but the build resolved '{resolved.Identity}'.");
            }
        }
    }

    private static AssemblyInputException Invalid(string origin, string reason) =>
        new(AssemblyInputProblem.InvalidAssembly, $"Bundle input '{origin}' {reason}.");
}
