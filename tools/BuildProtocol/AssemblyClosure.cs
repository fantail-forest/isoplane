using System.Reflection;
using System.Reflection.Metadata;
using System.Reflection.PortableExecutable;

namespace Wisp.Build;

internal enum AssemblyInputProblem
{
    InvalidAssembly,
    DuplicateIdentity,
    UnsupportedCulture,
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
    internal static ManagedIdentity From(AssemblyName name) => new(name.Name ?? "", name.Version ?? new Version(0, 0, 0, 0),
        name.CultureName ?? "", Convert.ToHexStringLower(name.GetPublicKeyToken() ?? []));
    public override string ToString() => $"{Name}, Version={Version}, Culture={(Culture.Length == 0 ? "neutral" : Culture)}, PublicKeyToken={(PublicKeyToken.Length == 0 ? "null" : PublicKeyToken)}";
}

internal sealed record InspectedAssembly(string Origin, ManagedIdentity Identity, ManagedIdentity[] References, bool IlOnly);

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
            using var pe = new PEReader(input, PEStreamOptions.LeaveOpen | PEStreamOptions.PrefetchMetadata);
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
            if (identity.Name.Length == 0 || identity.Name.IndexOfAny(['/', '\\']) >= 0)
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
            return new(origin, identity, references, header.Flags.HasFlag(CorFlags.ILOnly));
        }
        catch (BadImageFormatException) { throw Invalid(origin, "is not a valid managed assembly"); }
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
            if (application && !assembly.IlOnly) throw Invalid(assembly.Origin, "must contain managed IL only");
            if (assembly.Identity.Culture.Length != 0)
                throw new AssemblyInputException(AssemblyInputProblem.UnsupportedCulture, $"Satellite assembly '{assembly.Identity.Name}' requires deployment asset support.");
            if (!assemblies.TryAdd(assembly.Identity.Name, assembly))
                throw new AssemblyInputException(AssemblyInputProblem.DuplicateIdentity,
                    $"Conflicting inputs define assembly '{assembly.Identity.Name}': '{assemblies[assembly.Identity.Name].Origin}' and '{assembly.Origin}'.");
        }
        return assemblies;
    }

    internal static void Validate(IReadOnlyDictionary<string, InspectedAssembly> application,
        IReadOnlyDictionary<string, InspectedAssembly> platform)
    {
        foreach (var assembly in application.Values)
        {
            if (platform.ContainsKey(assembly.Identity.Name))
                throw new AssemblyInputException(AssemblyInputProblem.PlatformSubstitution,
                    $"Application input attempts to replace platform assembly '{assembly.Identity.Name}'. Remove it from the bundle; the selected runtime supplies it.");
            foreach (var reference in assembly.References)
            {
                if (!application.TryGetValue(reference.Name, out var resolved) && !platform.TryGetValue(reference.Name, out resolved))
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
