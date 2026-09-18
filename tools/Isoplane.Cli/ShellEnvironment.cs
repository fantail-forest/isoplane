namespace Wisp.Cli;

/// <summary>
/// The shell configuration that makes the installed SDK and the CLI discoverable in new terminals: one sourced
/// environment file under the Isoplane home, and one line in each shell startup file that sources it.
/// </summary>
internal static class ShellEnvironment
{
    internal const string Marker = "# isoplane setup";

    internal static string EnvironmentFile(string home) => Path.Combine(home, "env");

    /// <summary>The environment script: the selected SDK, then the standard .NET tools directory, first on PATH.</summary>
    internal static string Script(string home, string userHome)
    {
        var configured = Path.GetFullPath(home) == Path.GetFullPath(Path.Combine(userHome, ".isoplane"));
        var lines = new List<string> { "# Isoplane SDK environment; isoplane setup rewrites this file." };
        if (!configured)
            lines.Add($"export ISOPLANE_HOME=\"{home}\"");
        var isoplaneHome = configured ? "$HOME/.isoplane" : home;
        foreach (var directory in new[] { "$HOME/.dotnet/tools", isoplaneHome + "/dotnet" })
            lines.Add($"case \":$PATH:\" in *\":{directory}:\"*) ;; *) export PATH=\"{directory}:$PATH\" ;; esac");
        return string.Join('\n', lines) + "\n";
    }

    /// <summary>The line appended to a startup file. It names the file through <c>$HOME</c> when it lives there.</summary>
    internal static string SourceLine(string home, string userHome)
    {
        var file = EnvironmentFile(home);
        var user = Path.GetFullPath(userHome).TrimEnd(Path.DirectorySeparatorChar) + Path.DirectorySeparatorChar;
        var reference = file.StartsWith(user, StringComparison.Ordinal) ? "$HOME/" + file[user.Length..] : file;
        return $". \"{reference}\" {Marker}";
    }

    /// <summary>Login shells read <c>.profile</c> (or <c>.bash_profile</c> when present); terminal windows read <c>.bashrc</c> or <c>.zshrc</c>.</summary>
    internal static IReadOnlyList<string> StartupFiles(string userHome)
    {
        var files = new List<string> { Path.Combine(userHome, ".profile") };
        foreach (var optional in new[] { ".bash_profile", ".bashrc", ".zshrc" })
        {
            var path = Path.Combine(userHome, optional);
            if (File.Exists(path))
                files.Add(path);
        }
        return files;
    }

    /// <summary>Appends the line unless the file already carries it; returns whether this call added it.</summary>
    internal static bool AddLine(string file, string line)
    {
        var existing = File.Exists(file) ? File.ReadAllText(file) : "";
        if (existing.Split('\n').Any(candidate => candidate.Trim() == line))
            return false;
        var separator = existing.Length == 0 || existing.EndsWith('\n') ? "" : "\n";
        File.AppendAllText(file, $"{separator}{line}\n");
        return true;
    }

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
