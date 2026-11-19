namespace Isoplane.Cli;

/// <summary>The fields of a Linux machine's os-release file, for every part of the CLI that asks which distribution it runs on.</summary>
internal static class OsRelease
{
    // The os-release specification's order: the second is read only when the first is missing.
    internal static readonly string[] Files = ["/etc/os-release", "/usr/lib/os-release"];

    /// <summary>
    /// The fields of the first of <paramref name="files"/> that exists, or null when none exists or that
    /// file cannot be read: a file that is there and unreadable leaves the distribution unknown.
    /// </summary>
    internal static IReadOnlyDictionary<string, string>? Read(IEnumerable<string> files)
    {
        foreach (var file in files)
        {
            try { return Parse(File.ReadLines(file)); }
            catch (Exception error) when (error is FileNotFoundException or DirectoryNotFoundException) { }
            catch (Exception error) when (error is IOException or UnauthorizedAccessException) { return null; }
        }
        return null;
    }

    /// <summary>Each assigned name with its value unquoted. The file is a shell fragment, so a name's last assignment is its value.</summary>
    internal static IReadOnlyDictionary<string, string> Parse(IEnumerable<string> lines)
    {
        var fields = new Dictionary<string, string>(StringComparer.Ordinal);
        foreach (var parts in lines.Select(line => line.Split('=', 2)).Where(parts => parts.Length == 2))
            fields[parts[0].Trim()] = parts[1].Trim().Trim('"', '\'');
        return fields;
    }
}
