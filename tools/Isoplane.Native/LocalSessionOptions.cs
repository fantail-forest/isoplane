using Isoplane.Native.Interop;

namespace Isoplane.Native;

/// <summary>The manifest an application runs under locally: its name, and the limits that replace the defaults.</summary>
internal sealed record ApplicationManifest(string Name, LimitOverrides Limits)
{
    internal NativeManifest Lend(NativeArguments arguments) => new() { name = arguments.Text(Name), limits = Limits };
}

/// <summary>What a local session runs and where it keeps its files.</summary>
/// <param name="StateDirectory">The absolute directory that holds the session's lock and state.</param>
/// <param name="RuntimeLibrary">The native library, whose identity the compiled runtime depends on.</param>
/// <param name="CompilationDirectory">The absolute directory for the project's prepared application.</param>
/// <param name="RuntimeCacheDirectory">The absolute directory of the per-user cache of compiled runtimes.</param>
/// <param name="MachineRoots">The machine's trusted roots exported as PEM, or null to trust the shipped roots alone.</param>
/// <param name="Configuration">The application's settings by name.</param>
/// <param name="LogFilter">The filter that admits host log records, in <c>RUST_LOG</c> syntax.</param>
/// <param name="Port">The loopback port to listen on, or null for the default.</param>
/// <param name="DebugEndpoint">Where the guest's debugger agent connects, or null to run without a debugger.</param>
/// <param name="Package">The absolute path of the application package the shared runtime loads.</param>
internal sealed record LocalSessionOptions(ApplicationManifest Manifest, string StateDirectory, string RuntimeLibrary, string CompilationDirectory,
    string RuntimeCacheDirectory, string? MachineRoots, IReadOnlyDictionary<string, string> Configuration, string LogFilter, ushort? Port,
    string? DebugEndpoint, string Package)
{
    internal unsafe NativeSessionOptions Lend(NativeArguments arguments) => new()
    {
        manifest = Manifest.Lend(arguments),
        state_directory = arguments.Text(StateDirectory),
        runtime_library = arguments.Text(RuntimeLibrary),
        compilation_directory = arguments.Text(CompilationDirectory),
        runtime_cache_directory = arguments.Text(RuntimeCacheDirectory),
        machine_roots = arguments.Text(MachineRoots),
        configuration = arguments.Settings(Configuration),
        configuration_length = (nuint)Configuration.Count,
        log_filter = arguments.Text(LogFilter),
        port_set = Port is not null,
        port = Port ?? 0,
        debug_endpoint = arguments.Text(DebugEndpoint),
        package = arguments.Text(Package),
    };
}
