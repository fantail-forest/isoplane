using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using Isoplane.Native;

namespace Isoplane.Cli;

/// <summary>
/// Where a project's local build and session files go: its package, session state and scratch.
/// They live in the project's <c>.isoplane</c> directory, or in a folder of the project's own
/// under <paramref name="targetDirectory"/> when one is given.
/// </summary>
internal sealed class LocalProject(string directory, string? targetDirectory)
{
    private readonly string directory = Path.GetFullPath(directory);

    /// <summary>The project at <paramref name="directory"/>, relocated when <c>ISOPLANE_TARGET_DIR</c> is set.</summary>
    internal LocalProject(string directory) : this(directory, CliPaths.TargetDirectory) { }

    private string Root => targetDirectory is null ? Path.Combine(directory, ".isoplane") : Relocated(targetDirectory);

    /// <summary>Where <c>isoplane dev</c> builds an interpreted project's package.</summary>
    internal string Package => Path.Combine(Root, "package", "application.tar");

    /// <summary>Where the session keeps the trusted roots it exports for the guest.</summary>
    internal string Trust => Path.Combine(Root, "local", "trust");

    /// <summary>A new directory name of this project's for one build or debug session's temporary files of the given <paramref name="kind"/>.</summary>
    internal string Scratch(string kind) => Path.Combine(Root, kind, Guid.NewGuid().ToString("N"));

    // Named for the project and its location, so a target directory shared by every project
    // keeps two projects with the same folder name apart.
    private string Relocated(string target)
    {
        var location = OperatingSystem.IsWindows() ? directory.ToUpperInvariant() : directory;
        var folder = Path.Combine(target, Path.GetFileName(directory) + "-" + Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(location)))[..16]);
        var relative = Path.GetRelativePath(directory, folder);
        var outside = Path.IsPathRooted(relative) || relative == ".." || relative.StartsWith(".." + Path.DirectorySeparatorChar, StringComparison.Ordinal);
        // A session watching the project for source edits would see its own output there and rebuild without end.
        return outside ? folder
            : throw new InvalidOperationException($"ISOPLANE_TARGET_DIR must be outside the project; '{target}' is inside '{directory}'.");
    }

    internal byte[] SessionOptions(DevelopmentSettings settings, string? debugEndpoint = null, string? debugDirectory = null, string? package = null)
    {
        var identity = Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(settings.Name)));
        Dictionary<string, object?> options = new()
        {
            ["manifest"] = Manifest(settings.Name, settings.Limits),
            ["state_directory"] = debugDirectory ?? Path.Combine(Root, "local", identity),
            ["runtime_library"] = NativeRuntime.LibraryPath,
            ["compilation_directory"] = targetDirectory is null ? Path.Combine(settings.IntermediateDirectory, "isoplane") : Path.Combine(Root, "obj"),
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

    /// <summary>
    /// The manifest a local session uses, with explicit numeric budgets and
    /// <paramref name="overrides"/> applied to them. Runtime preparation uses the same manifest.
    /// </summary>
    internal static Dictionary<string, object?> Manifest(string name, LimitOverrides overrides)
    {
        Dictionary<string, object?> manifest = new() { ["name"] = name, ["limits"] = new Dictionary<string, object>
            {
                ["instances_per_application"] = 16UL,
                ["module_bytes_compressed"] = 52428800UL,
                ["package_bytes"] = 52428800UL,
                ["instance_memory_max"] = 536870912UL,
                ["occupancy_ms_per_dispatch"] = 300000UL,
                ["subrequests_per_dispatch"] = 10000UL,
                ["pending_futures_per_dispatch"] = 4096UL,
            } };
        if (overrides.MemoryBytes is { } memory) manifest["guest_memory_bytes"] = memory;
        if (overrides.ResourceLimits is { Count: > 0 } limits) manifest["resource_limits"] = limits;
        return manifest;
    }
}
