using System.Text.Json;
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
            Dictionary<string, object?> options = new()
            {
                ["manifest"] = LocalProject.Manifest("runtime", LimitOverrides.None),
                ["runtime_library"] = NativeRuntime.LibraryPath,
                ["runtime_cache_directory"] = cacheDirectory,
                ["runtime_identity"] = File.ReadAllText(runtime.Identity),
            };
            if (MachineTrust.Export(trust.FullName) is { } roots) options["machine_roots"] = roots;
            var report = RuntimeCache.Prepare(File.ReadAllBytes(runtime.Module), File.ReadAllBytes(runtime.DebugModule),
                JsonSerializer.SerializeToUtf8Bytes(options));
            output.WriteLine($"Compiled runtime cache: {cacheDirectory}");
            output.WriteLine(report["baseline_compiled"]!.GetValue<bool>() ? "Compiled the runtime." : "The runtime was already compiled.");
            output.WriteLine(report["debug_compiled"]!.GetValue<bool>() ? "Compiled the debug runtime." : "The debug runtime was already compiled.");
            return 0;
        }
        finally
        {
            trust.Delete(recursive: true);
        }
    }
}
