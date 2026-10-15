using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol;
using Mono.Debugger.Soft;
using Mono.Cecil;

namespace Isoplane.Debugging;

internal sealed class GuestCode(VirtualMachine machine)
{
    private string? packageRoot;
    private readonly Dictionary<AssemblyMirror, HashSet<int>> hiddenMethods = [];

    private bool InPackage(AssemblyMirror assembly)
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
        return assembly.Location.StartsWith(packageRoot, StringComparison.Ordinal);
    }

    internal static string PackageRoot(ModuleDefinition module)
    {
        var definition = module.GetType("Isoplane.MonoRuntime.PackageLoader");
        if (definition?.Fields.SingleOrDefault(field => field.Name == "Root")?.Constant is not string value)
            throw new ProtocolException("The guest runtime does not expose its application package location.");
        return value.TrimEnd('/') + "/";
    }

    /// <summary>Whether <paramref name="method"/> is application code: loaded from the package and not marked as generated or non-user code.</summary>
    internal bool Contains(MethodMirror method)
    {
        var assembly = method.DeclaringType.Assembly;
        return InPackage(assembly) && !HiddenIn(assembly, method);
    }

    /// <summary>
    /// The positions in <paramref name="stack"/> of the frames the guest's call stack shows: application, SDK and
    /// framework code down to the outermost application or SDK frame, without the runtime machinery among them.
    /// The frames below the outermost application or SDK frame are the runtime invoking the application.
    /// </summary>
    internal List<int> Shown(IReadOnlyList<Mono.Debugger.Soft.StackFrame> stack)
    {
        int outermost = stack.Count - 1;
        while (outermost >= 0 && !(stack[outermost].Method is { } method && Anchors(method))) outermost--;
        return Enumerable.Range(0, outermost + 1).Where(index => stack[index].Method is { } method && Shows(method)).ToList();
    }

    /// <summary>Whether a call stack shows <paramref name="method"/>, apart from the frames <see cref="Shown"/> trims: package code by <see cref="Contains"/>, SDK and framework code, but not runtime machinery.</summary>
    internal bool Shows(MethodMirror method)
    {
        var assembly = method.DeclaringType.Assembly;
        return InPackage(assembly) ? !HiddenIn(assembly, method) : !IsMachinery(assembly.GetName().Name, method.DeclaringType.FullName);
    }

    private bool Anchors(MethodMirror method)
    {
        var assembly = method.DeclaringType.Assembly;
        if (InPackage(assembly)) return true;
        string? name = assembly.GetName().Name;
        return IsIsoplane(name) && !IsMachinery(name, method.DeclaringType.FullName);
    }

    // The package loader refuses a package assembly whose name the runtime supplies,
    // so outside the package these names belong only to the runtime's own assemblies.
    private static bool IsIsoplane(string? assembly) => assembly?.StartsWith("Isoplane.", StringComparison.Ordinal) == true;

    // Isoplane.MonoRuntime loads the package and exports the host interface, and Isoplane.Sdk's Abi and
    // Runtime namespaces bind that interface and run the guest's dispatch and scheduling. The rest of
    // Isoplane.Sdk and Isoplane.AspNetCore is the API applications call and are called from.
    private static bool IsMachinery(string? assembly, string type) => assembly == "Isoplane.MonoRuntime"
        || IsIsoplane(assembly) && (type.StartsWith("Isoplane.Abi.", StringComparison.Ordinal) || type.StartsWith("Isoplane.Runtime.", StringComparison.Ordinal));

    private bool HiddenIn(AssemblyMirror assembly, MethodMirror method)
    {
        if (!hiddenMethods.TryGetValue(assembly, out var hidden))
        {
            using var bytes = new MemoryStream(assembly.GetMetadataBlob());
            using var metadata = AssemblyDefinition.ReadAssembly(bytes);
            hidden = metadata.MainModule.GetTypes().SelectMany(type => type.Methods).Where(Hidden)
                .Select(candidate => candidate.MetadataToken.ToInt32()).ToHashSet();
            hiddenMethods.Add(assembly, hidden);
        }
        return hidden.Contains(method.MetadataToken);
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
