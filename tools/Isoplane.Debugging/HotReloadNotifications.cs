using Isoplane.Build;
using Mono.Debugger.Soft;

namespace Isoplane.Debugging;

/// <summary>Runs the application's metadata update handlers inside the paused guest.</summary>
internal static class HotReloadNotifications
{
    private const string Sdk = "Isoplane.Sdk";
    private const string Invoker = "Isoplane.Runtime.HotReloadHandlers";

    /// <summary>Calls the guest SDK's handler invoker with the updated types and returns the warnings it reports.</summary>
    internal static async Task<string[]> ApplyAsync(VirtualMachine machine, ModuleUpdate[] updates, CancellationToken cancellation)
    {
        if (updates.Length == 0) return [];
        var thread = machine.GetThreads().Single();
        var assemblies = machine.RootDomain.GetAssemblies();
        var sdk = assemblies.SingleOrDefault(assembly => assembly.GetName().Name == Sdk)
            ?? throw new TypeLoadException($"The guest has not loaded '{Sdk}', which runs its Hot Reload handlers.");
        var invoker = RequireType(sdk, Invoker);
        var notify = invoker.GetMethod("NotifyLoadedAssemblies")
            ?? throw new MissingMethodException($"The guest's '{Invoker}' has no 'NotifyLoadedAssemblies' method. Rebuild the application with the SDK version this debugger belongs to.");
        var modules = assemblies.ToDictionary(assembly => assembly.ManifestModule.ModuleVersionId);
        Value[] types = updates.SelectMany(update => update.UpdatedTypes
            .Select(token => (Value)modules[update.Module].GetType((uint)token).GetTypeObject())).ToArray();
        var arrayType = RequireType(machine.RootDomain.Corlib, "System.Array");
        var create = arrayType.GetMethods().Single(method => method.Name == "CreateInstance" && method.IsStatic
            && method.GetParameters() is [var element, var count]
            && element.ParameterType.FullName == "System.Type" && count.ParameterType.FullName == "System.Int32");
        var created = await InvokeAsync(arrayType, thread, create,
            [RequireType(machine.RootDomain.Corlib, "System.Type").GetTypeObject(), machine.CreateValue(types.Length)], cancellation);
        if (created is not ArrayMirror changed)
            throw new InvalidOperationException($"Creating the changed-type array returned {created.GetType().Name}: {created}.");
        changed.SetValues(0, types);
        var reported = await InvokeAsync(invoker, thread, notify, [changed], cancellation);
        if (reported is not ArrayMirror warnings)
            throw new InvalidOperationException($"Notifying the Hot Reload handlers returned {reported.GetType().Name}: {reported}.");
        return warnings.GetValues(0, warnings.Length).Cast<StringMirror>().Select(warning => warning.Value).ToArray();
    }

    private static TypeMirror RequireType(AssemblyMirror assembly, string name) =>
        assembly.GetType(name) ?? throw new TypeLoadException($"The guest assembly '{assembly.GetName().Name}' does not contain '{name}'.");

    private static async Task<Value> InvokeAsync(IInvokable receiver, ThreadMirror thread, MethodMirror method,
        Value[] arguments, CancellationToken cancellation) =>
        await receiver.InvokeMethodAsync(thread, method, arguments, InvokeOptions.SingleThreaded | InvokeOptions.DisableBreakpoints)
            .WaitAsync(cancellation);
}
