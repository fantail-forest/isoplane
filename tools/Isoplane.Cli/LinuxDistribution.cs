namespace Isoplane.Cli;

/// <summary>The Linux distribution identifier and optional version from os-release.</summary>
internal sealed record LinuxDistribution(string? Id, string? VersionId)
{
    // The os-release specification's order: the second is read only when the first is missing.
    internal static readonly string[] OsReleaseFiles = ["/etc/os-release", "/usr/lib/os-release"];

    /// <summary>The distribution of a machine whose os-release file is missing or cannot be read.</summary>
    internal static readonly LinuxDistribution Unknown = new(null, null);

    /// <summary>
    /// What the first of <paramref name="osReleaseFiles"/> that exists states, or <see cref="Unknown"/> when
    /// none exists or that file cannot be read: a file that is there and unreadable is not passed over for the next.
    /// </summary>
    internal static LinuxDistribution Read(IEnumerable<string> osReleaseFiles)
    {
        foreach (var file in osReleaseFiles)
        {
            try { return Parse(File.ReadLines(file)); }
            catch (Exception error) when (error is FileNotFoundException or DirectoryNotFoundException) { }
            catch (Exception error) when (error is IOException or UnauthorizedAccessException) { return Unknown; }
        }
        return Unknown;
    }

    /// <summary>
    /// What an os-release file's lines state, with values unquoted. The file is a shell fragment, so a name's
    /// last assignment is its value, and one that assigns no <c>ID</c> has the specification's default, <c>linux</c>.
    /// </summary>
    internal static LinuxDistribution Parse(IEnumerable<string> lines)
    {
        var distribution = new LinuxDistribution("linux", null);
        foreach (var parts in lines.Select(line => line.Split('=', 2)).Where(parts => parts.Length == 2))
        {
            var value = parts[1].Trim().Trim('"', '\'');
            distribution = parts[0].Trim() switch
            {
                "ID" => distribution with { Id = value },
                "VERSION_ID" => distribution with { VersionId = value },
                _ => distribution
            };
        }
        return distribution;
    }
}

internal static class LinuxOperatingSystem
{
    extension(OperatingSystem)
    {
        /// <summary>The distribution this machine runs, or null when it does not run Linux.</summary>
        internal static LinuxDistribution? LinuxDistribution() =>
            OperatingSystem.IsLinux() ? Cli.LinuxDistribution.Read(Cli.LinuxDistribution.OsReleaseFiles) : null;
    }
}
