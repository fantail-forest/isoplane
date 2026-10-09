using System.Runtime.InteropServices;

namespace Isoplane.Cli;

internal static class CliPaths
{
    internal static string Home => Environment.GetEnvironmentVariable("ISOPLANE_HOME") is { Length: > 0 } configured
        ? Path.GetFullPath(configured)
        : Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".isoplane");

    /// <summary>
    /// Where Isoplane keeps what it can build again and every project of this user shares: under
    /// <c>ISOPLANE_HOME</c> when that is set, otherwise in the platform's per-user cache location.
    /// </summary>
    internal static string Cache
    {
        get
        {
            if (Environment.GetEnvironmentVariable("ISOPLANE_HOME") is { Length: > 0 }) return Path.Combine(Home, "cache");
            if (OperatingSystem.IsWindows())
                return Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Isoplane", "Cache");
            var profile = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
            if (OperatingSystem.IsMacOS()) return Path.Combine(profile, "Library", "Caches", "isoplane");
            // The XDG base directory specification ignores a relative XDG_CACHE_HOME.
            var caches = Environment.GetEnvironmentVariable("XDG_CACHE_HOME") is { Length: > 0 } configured && Path.IsPathRooted(configured)
                ? configured : Path.Combine(profile, ".cache");
            return Path.Combine(caches, "isoplane");
        }
    }

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
