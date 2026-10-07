using System.Runtime.InteropServices;

namespace Isoplane.Cli;

internal static class CliPaths
{
    internal static string Home => Environment.GetEnvironmentVariable("ISOPLANE_HOME") is { Length: > 0 } configured
        ? Path.GetFullPath(configured)
        : Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".isoplane");
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
