using System.Security.Cryptography;
using System.Text;
using Isoplane.Native;
using Isoplane.Native.Interop;

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

    /// <summary>The directory containing this project's CLI-owned build and session outputs.</summary>
    internal string Root => targetDirectory is null ? Path.Combine(directory, ".isoplane") : Relocated(targetDirectory);

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

    internal LocalSessionOptions SessionOptions(DevelopmentSettings settings, string? debugEndpoint = null, string? debugDirectory = null, string? package = null)
    {
        var identity = Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(settings.Name)));
        return new(
            Manifest: Manifest(settings.Name, settings.Limits),
            StateDirectory: debugDirectory ?? Path.Combine(Root, "local", identity),
            RuntimeLibrary: NativeRuntime.LibraryPath,
            CompilationDirectory: targetDirectory is null ? Path.Combine(settings.IntermediateDirectory, "isoplane") : Path.Combine(Root, "obj"),
            RuntimeCacheDirectory: settings.RuntimeCacheDirectory,
            MachineRoots: settings.MachineRoots,
            Configuration: settings.Configuration,
            LogFilter: Environment.GetEnvironmentVariable("RUST_LOG") ?? "info",
            Port: settings.Port,
            DebugEndpoint: debugEndpoint,
            Package: package ?? Package);
    }

    /// <summary>
    /// The manifest a development session runs an application under. A session uses the default resource budgets unless <paramref name="overrides"/>
    /// replace them. The compiled runtime depends on these limits, so <c>isoplane prepare-runtime</c>
    /// uses them too.
    /// </summary>
    internal static ApplicationManifest Manifest(string name, LimitOverrides overrides) => new(name, overrides);
}
