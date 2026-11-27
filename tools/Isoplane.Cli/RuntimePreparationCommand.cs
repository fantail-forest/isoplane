using Isoplane.Native;

namespace Isoplane.Cli;

/// <summary>
/// <c>isoplane prepare-runtime --runtime DIRECTORY</c>: compiles a shared runtime into the runtime
/// cache ahead of the first session on it, so that session starts without compiling it.
/// </summary>
internal static class RuntimePreparationCommand
{
    internal static int Run(string[] arguments, string cacheDirectory, TextWriter output)
    {
        var parsed = CliCommandLine.Parse(arguments);
        var runtime = new SharedRuntime(Path.GetFullPath(parsed.GetValue<string>("--runtime")!));
        // Sessions guest-read the machine's roots, and the compiled runtime depends on them.
        var trust = Directory.CreateTempSubdirectory("isoplane-prepare-runtime-");
        try
        {
            var prepared = RuntimeCache.Prepare(File.ReadAllBytes(runtime.Module), File.ReadAllBytes(runtime.DebugModule),
                new RuntimePreparationOptions(LocalProject.Manifest("runtime", default), NativeRuntime.LibraryPath, cacheDirectory,
                    MachineTrust.Export(trust.FullName), File.ReadAllText(runtime.Identity)));
            output.WriteLine($"Compiled runtime cache: {cacheDirectory}");
            output.WriteLine(prepared.baseline_compiled ? "Compiled the runtime." : "The runtime was already compiled.");
            output.WriteLine(prepared.debug_compiled ? "Compiled the debug runtime." : "The debug runtime was already compiled.");
            return 0;
        }
        finally
        {
            trust.Delete(recursive: true);
        }
    }
}
