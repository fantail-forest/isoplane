using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol;
using Mono.Debugger.Soft;
using Mono.Cecil;

namespace Isoplane.Debugging;

internal sealed class GuestCode(VirtualMachine machine)
{
    private string? packageRoot;
    private readonly Dictionary<AssemblyMirror, HashSet<int>> hiddenMethods = [];

    private bool Contains(string module)
    {
        if (packageRoot is null)
        {
            var runtime = machine.RootDomain.GetAssemblies().SingleOrDefault(assembly => assembly.GetName().Name == "Isoplane.MonoRuntime")
                ?? throw new ProtocolException("The guest runtime does not expose its application package loader.");
            // Live type lookup can invoke the guest's resolver and invalidate paused frames.
            using var bytes = new MemoryStream(runtime.GetMetadataBlob());
            using var metadata = AssemblyDefinition.ReadAssembly(bytes);
            packageRoot = PackageRoot(metadata.MainModule);
        }
        return module.StartsWith(packageRoot, StringComparison.Ordinal);
    }

    internal static string PackageRoot(ModuleDefinition module)
    {
        var definition = module.GetType("Isoplane.MonoRuntime.PackageLoader");
        if (definition?.Fields.SingleOrDefault(field => field.Name == "Root")?.Constant is not string value)
            throw new ProtocolException("The guest runtime does not expose its application package location.");
        return value.TrimEnd('/') + "/";
    }

    internal bool Contains(MethodMirror method)
    {
        var assembly = method.DeclaringType.Assembly;
        if (!Contains(assembly.Location)) return false;
        if (!hiddenMethods.TryGetValue(assembly, out var hidden))
        {
            using var bytes = new MemoryStream(assembly.GetMetadataBlob());
            using var metadata = AssemblyDefinition.ReadAssembly(bytes);
            hidden = metadata.MainModule.GetTypes().SelectMany(type => type.Methods).Where(Hidden)
                .Select(candidate => candidate.MetadataToken.ToInt32()).ToHashSet();
            hiddenMethods.Add(assembly, hidden);
        }
        return !hidden.Contains(method.MetadataToken);
    }

    internal static bool Hidden(MethodDefinition method)
    {
        if (Hidden(method.CustomAttributes)) return true;
        for (var type = method.DeclaringType; type is not null; type = type.DeclaringType)
            if (Hidden(type.CustomAttributes)) return true;
        return false;
    }

    private static bool Hidden(IEnumerable<CustomAttribute> attributes) => attributes.Any(attribute =>
        attribute.AttributeType.FullName is "System.CodeDom.Compiler.GeneratedCodeAttribute"
            or "System.Diagnostics.DebuggerNonUserCodeAttribute" or "System.Diagnostics.DebuggerHiddenAttribute");
}
