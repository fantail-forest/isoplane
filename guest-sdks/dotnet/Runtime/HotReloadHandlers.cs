using System;
using System.Collections.Generic;
using System.Reflection;

namespace Isoplane.Runtime;

/// <summary>Runs the metadata update handlers that loaded assemblies declare, for the debugger to call in a paused guest after a Hot Reload edit.</summary>
internal static class HotReloadHandlers
{
    private const string Attribute = "System.Reflection.Metadata.MetadataUpdateHandlerAttribute";
    private const string ClearCache = "ClearCache";
    private const string UpdateApplication = "UpdateApplication";
    private const BindingFlags Static = BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Static;

    /// <summary>Notifies the handlers of every loaded assembly, as <see cref="Notify"/> does, and returns its warnings.</summary>
    internal static string[] NotifyLoadedAssemblies(Type[] updatedTypes) =>
        Notify(AppDomain.CurrentDomain.GetAssemblies(), updatedTypes);

    /// <summary>
    /// Calls <c>ClearCache</c> on every handler and then <c>UpdateApplication</c> on every handler, each time
    /// taking an assembly's handlers after those of the assemblies it references. Returns one warning for each
    /// handler that threw, that declares neither method or one of the wrong shape, and for each assembly whose
    /// handlers could not be read. The remaining handlers still run.
    /// </summary>
    internal static string[] Notify(Assembly[] assemblies, Type[] updatedTypes)
    {
        var warnings = new List<string>();
        var clears = new List<MethodInfo>();
        var updates = new List<MethodInfo>();
        foreach (var assembly in DependenciesFirst(assemblies))
        {
            try
            {
                foreach (var handler in DeclaredHandlers(assembly, warnings))
                {
                    var clear = Find(handler, ClearCache, warnings);
                    var update = Find(handler, UpdateApplication, warnings);
                    if (clear is not null) clears.Add(clear);
                    if (update is not null) updates.Add(update);
                    // The .NET SDK's watcher delivers static asset changes to UpdateContent, which this debugger
                    // does not produce, so a handler for those alone is not misdeclared.
                    if (clear is null && update is null && Named(handler, "UpdateContent") is null)
                        warnings.Add($"Hot Reload handler '{handler.AssemblyQualifiedName}' declares neither a static '{ClearCache}' nor a static '{UpdateApplication}' method.");
                }
            }
            // Reading another assembly's attributes and handler types loads whatever they name, which can fail
            // in ways this assembly cannot enumerate; the other assemblies' handlers still need to run.
            catch (Exception error)
            {
                warnings.Add($"The Hot Reload handlers of '{assembly.FullName}' could not be read: {error}");
            }
        }
        foreach (var clear in clears) Call(clear, updatedTypes, warnings);
        foreach (var update in updates) Call(update, updatedTypes, warnings);
        return [.. warnings];
    }

    private static void Call(MethodInfo method, Type[] updatedTypes, List<string> warnings)
    {
        try { method.CreateDelegate<Action<Type[]?>>()(updatedTypes); }
        // A handler is application or library code, and one that fails leaves the others' caches to clear.
        catch (Exception error)
        {
            warnings.Add($"Hot Reload handler '{method.DeclaringType!.FullName}.{method.Name}' failed: {error}");
        }
    }

    private static List<Type> DeclaredHandlers(Assembly assembly, List<string> warnings)
    {
        var handlers = new List<Type>();
        foreach (var attribute in assembly.GetCustomAttributesData())
        {
            // Matching by name accepts the copy of the attribute a netstandard library declares for itself.
            if (attribute.AttributeType.FullName != Attribute) continue;
            if (attribute.ConstructorArguments is [{ Value: Type handler }]) handlers.Add(handler);
            else warnings.Add($"'{attribute}' on '{assembly.FullName}' does not name a Hot Reload handler type.");
        }
        return handlers;
    }

    private static MethodInfo? Find(Type handler, string name, List<string> warnings)
    {
        if (handler.GetMethod(name, Static, binder: null, [typeof(Type[])], modifiers: null) is { } method
            && method.ReturnType == typeof(void))
            return method;
        if (Named(handler, name) is { } mismatched)
            warnings.Add($"Hot Reload handler '{handler.FullName}' declares '{mismatched}', which is not a static 'void {name}(Type[])' method.");
        return null;
    }

    private static MethodInfo? Named(Type handler, string name)
    {
        foreach (var method in handler.GetMethods(Static | BindingFlags.Instance))
            if (method.Name == name) return method;
        return null;
    }

    private static List<Assembly> DependenciesFirst(Assembly[] assemblies)
    {
        var loaded = new Dictionary<string, Assembly>(StringComparer.OrdinalIgnoreCase);
        foreach (var assembly in assemblies)
            if (assembly.GetName().Name is { } name) loaded.TryAdd(name, assembly);
        var ordered = new List<Assembly>(assemblies.Length);
        var visited = new HashSet<Assembly>();
        foreach (var assembly in assemblies) Visit(assembly);
        return ordered;

        void Visit(Assembly assembly)
        {
            if (!visited.Add(assembly)) return;
            foreach (var reference in assembly.GetReferencedAssemblies())
                if (reference.Name is { } name && loaded.TryGetValue(name, out var dependency)) Visit(dependency);
            ordered.Add(assembly);
        }
    }
}
