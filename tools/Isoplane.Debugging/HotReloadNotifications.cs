using Isoplane.Build;
using Mono.Debugger.Soft;

namespace Isoplane.Debugging;

/// <summary>Runs the project's SDK metadata update handlers inside the paused guest.</summary>
internal sealed class HotReloadNotifications(byte[] agent)
{
    private const string AssemblyName = "Microsoft.Extensions.DotNetDeltaApplier";
    private const string Namespace = "Microsoft.DotNet.HotReload.";
    private GuestAgent? guest;

    private sealed record GuestAgent(ObjectMirror Invoker, ObjectMirror Reporter);

    internal static HotReloadNotifications Read(string sdkDirectory)
    {
        string directory = Path.Combine(sdkDirectory, "DotnetTools", "dotnet-watch");
        string[] candidates = Directory.Exists(directory)
            ? Directory.GetFiles(directory, AssemblyName + ".dll", SearchOption.AllDirectories)
                .Where(path => Path.GetFileName(Path.GetDirectoryName(path)) == "net10.0"
                    && Path.GetFileName(Path.GetDirectoryName(Path.GetDirectoryName(path))) == "hotreload").ToArray()
            : [];
        if (candidates is not [var agentPath])
            throw new IOException($"Expected one .NET Hot Reload notification agent in '{directory}', found {candidates.Length}.");
        return new(File.ReadAllBytes(agentPath));
    }

    internal async Task<string[]> ApplyAsync(VirtualMachine machine, ModuleUpdate[] updates, CancellationToken cancellation)
    {
        if (updates.Length == 0) return [];
        var thread = machine.GetThreads().Single();
        var assemblies = machine.RootDomain.GetAssemblies();
        var loaded = FindAgent(machine);
        if (loaded is null)
        {
            var reflection = RequireType(machine.RootDomain.Corlib, "System.Reflection.Assembly");
            var load = reflection.GetMethods().Single(method => method.Name == "Load" && method.IsStatic
                && method.GetParameters() is [var parameter] && parameter.ParameterType.FullName == "System.Byte[]");
            await InvokeAsync(reflection, thread, load, [machine.RootDomain.CreateByteArray(agent)], cancellation);
            loaded = FindAgent(machine) ?? throw new TypeLoadException("The SDK Hot Reload notification agent did not load.");
        }

        var invokerType = RequireType(loaded, Namespace + "MetadataUpdateHandlerInvoker");
        if (guest is null)
        {
            var agentType = RequireType(loaded, Namespace + "HotReloadAgent");
            // The SDK agent owns the handler cache and invalidates it on AssemblyLoad.
            // Its event subscription also keeps the agent alive until this guest ends.
            var instance = (ObjectMirror)await InvokeAsync(agentType, thread, agentType.GetMethod(".ctor"),
                [machine.CreateValue(null), machine.CreateValue(null)], cancellation);
            guest = new((ObjectMirror)instance.GetValue(agentType.GetField("_metadataUpdateHandlerInvoker")),
                (ObjectMirror)await InvokeAsync(instance, thread, agentType.GetMethod("get_Reporter"), [], cancellation));
        }
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
        await InvokeAsync(guest.Invoker, thread, invokerType.GetMethod("MetadataUpdated"), [changed], cancellation);
        var reporterType = RequireType(loaded, Namespace + "AgentReporter");
        var logging = RequireType(loaded, Namespace + "ResponseLoggingLevel");
        var warnings = logging.GetValue(logging.GetField("WarningsAndErrors"));
        var messages = (ArrayMirror)await InvokeAsync(guest.Reporter, thread, reporterType.GetMethod("GetAndClearLogEntries"), [warnings], cancellation);
        return messages.GetValues(0, messages.Length).Cast<StructMirror>()
            .Select(entry => ((StringMirror)entry["Item1"]).Value).ToArray();
    }

    private static AssemblyMirror? FindAgent(VirtualMachine machine) =>
        machine.GetTypes(Namespace + "MetadataUpdateHandlerInvoker", false)
            .SingleOrDefault(type => type.Assembly.GetName().Name == AssemblyName)?.Assembly;

    private static TypeMirror RequireType(AssemblyMirror assembly, string name) =>
        assembly.GetType(name) ?? throw new TypeLoadException($"The guest assembly '{assembly.GetName().Name}' does not contain '{name}'.");

    private static async Task<Value> InvokeAsync(IInvokable receiver, ThreadMirror thread, MethodMirror method,
        Value[] arguments, CancellationToken cancellation) =>
        await receiver.InvokeMethodAsync(thread, method, arguments, InvokeOptions.SingleThreaded | InvokeOptions.DisableBreakpoints)
            .WaitAsync(cancellation);
}
