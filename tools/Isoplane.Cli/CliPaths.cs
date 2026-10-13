using System.Runtime.InteropServices;

namespace Isoplane.Cli;

internal static class CliPaths
{
    internal static string Home => Environment.GetEnvironmentVariable("ISOPLANE_HOME") is { Length: > 0 } configured
        ? Path.GetFullPath(configured)
        : Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".isoplane");

    /// <summary>
    /// Where Isoplane keeps what it can build again and every project of this user shares:
    /// <c>ISOPLANE_CACHE_DIR</c> when that is set, else under <c>ISOPLANE_HOME</c> when that is set,
    /// otherwise the platform's per-user cache location.
    /// </summary>
    internal static string Cache => CacheFor(Environment.GetEnvironmentVariable("ISOPLANE_CACHE_DIR"), Environment.GetEnvironmentVariable("ISOPLANE_HOME"));

    /// <summary>Where sessions keep the compiled shared runtimes, which <c>isoplane cache</c> reports and removes.</summary>
    internal static string RuntimeCache => Path.Combine(Cache, "runtimes");

    /// <summary>
    /// The directory given by <c>ISOPLANE_TARGET_DIR</c>, under which each project's local build and
    /// session files go instead of the project's own <c>.isoplane</c> directory; null when it is not set.
    /// </summary>
    internal static string? TargetDirectory => Environment.GetEnvironmentVariable("ISOPLANE_TARGET_DIR") is { Length: > 0 } configured
        ? Absolute("ISOPLANE_TARGET_DIR", configured) : null;

    internal static string CacheFor(string? cacheDirectory, string? home)
    {
        if (cacheDirectory is { Length: > 0 }) return Absolute("ISOPLANE_CACHE_DIR", cacheDirectory);
        if (home is { Length: > 0 }) return Path.Combine(Path.GetFullPath(home), "cache");
        if (OperatingSystem.IsWindows())
            return Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Isoplane", "Cache");
        var profile = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        if (OperatingSystem.IsMacOS()) return Path.Combine(profile, "Library", "Caches", "isoplane");
        // The XDG base directory specification ignores a relative XDG_CACHE_HOME.
        var caches = Environment.GetEnvironmentVariable("XDG_CACHE_HOME") is { Length: > 0 } configured && Path.IsPathRooted(configured)
            ? configured : Path.Combine(profile, ".cache");
        return Path.Combine(caches, "isoplane");
    }

    // A relative path would name a different directory from each working directory a session starts in.
    private static string Absolute(string variable, string value) => Path.IsPathFullyQualified(value)
        ? Path.GetFullPath(value)
        : throw new InvalidOperationException($"{variable} must be an absolute path, not '{value}'.");

    internal static string PlatformIdentifier()
    {
        var os = OperatingSystem.IsLinux()
            ? (RuntimeInformation.RuntimeIdentifier.Contains("musl", StringComparison.Ordinal) ? "linux-musl" : "linux")
            : OperatingSystem.IsWindows() ? "win" : OperatingSystem.IsMacOS() ? "osx" : "unsupported";
        var rid = os + "-" + RuntimeInformation.ProcessArchitecture.ToString().ToLowerInvariant();
        return rid;
    }

    internal static string Resolve(string directory, string relative)
    {
        if (string.IsNullOrWhiteSpace(relative) || Path.IsPathRooted(relative) || relative.Split('/', '\\').Any(part => part is ".." or "." or ""))
            throw new InvalidOperationException($"Unsafe release path: {relative}.");
        return Path.Combine(directory, relative);
    }

}
