using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using Isoplane.Native;

namespace Isoplane.Cli;

/// <summary>A project directory's local development state.</summary>
internal sealed class LocalProject(string directory)
{
    private readonly string directory = Path.GetFullPath(directory);

    /// <summary>Where <c>isoplane dev</c> builds an interpreted project's package.</summary>
    internal string Package => Path.Combine(directory, ".isoplane", "package", "application.tar");

    /// <summary>Where the session keeps the trusted roots it exports for the guest.</summary>
    internal string Trust => Path.Combine(directory, ".isoplane", "local", "trust");

    internal byte[] SessionOptions(DevelopmentSettings settings, string? debugEndpoint = null, string? debugDirectory = null, string? package = null)
    {
        var identity = Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(settings.Name)));
        // A development session is not held to a plan: it runs with the largest
        // limits the platform offers unless the launch profile narrows them.
        Dictionary<string, object?> manifest = new() { ["name"] = settings.Name, ["limits"] = new Dictionary<string, object>
            {
                ["module_bytes_compressed"] = 52428800UL,
                ["package_bytes"] = 52428800UL,
                ["instance_memory_max"] = 536870912UL,
                ["occupancy_ms_per_http_dispatch"] = 300000UL,
                ["subrequests_per_dispatch"] = 10000UL,
                ["pending_futures_per_dispatch"] = 4096UL,
            } };
        if (settings.Limits.MemoryBytes is { } memory) manifest["guest_memory_bytes"] = memory;
        if (settings.Limits.ResourceLimits is { Count: > 0 } limits) manifest["resource_limits"] = limits;
        Dictionary<string, object?> options = new()
        {
            ["manifest"] = manifest,
            ["state_directory"] = debugDirectory ?? Path.Combine(directory, ".isoplane", "local", identity),
            ["runtime_library"] = NativeRuntime.LibraryPath,
            ["compilation_directory"] = Path.Combine(settings.IntermediateDirectory, "isoplane"),
            ["runtime_cache_directory"] = settings.RuntimeCacheDirectory,
            ["configuration"] = settings.Configuration,
            ["port"] = settings.Port,
            ["log_filter"] = Environment.GetEnvironmentVariable("RUST_LOG") ?? "info",
            ["debug_endpoint"] = debugEndpoint,
            ["package"] = package ?? Package,
        };
        if (settings.MachineRoots is { } roots) options["machine_roots"] = roots;
        return JsonSerializer.SerializeToUtf8Bytes(options);
    }
}
