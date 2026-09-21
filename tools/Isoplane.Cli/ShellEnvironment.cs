namespace Isoplane.Cli;

/// <summary>Removes shell entries recorded by installations made before workload packaging.</summary>
internal static class ShellEnvironment
{
    /// <summary>Removes every copy of the line; returns whether the file changed. A missing file has nothing to remove.</summary>
    internal static bool RemoveLine(string file, string line)
    {
        if (!File.Exists(file))
            return false;
        var lines = File.ReadAllText(file).Split('\n');
        var kept = lines.Where(candidate => candidate.Trim() != line).ToArray();
        if (kept.Length == lines.Length)
            return false;
        File.WriteAllText(file, string.Join('\n', kept));
        return true;
    }
}
