using System.CommandLine;

namespace Isoplane.Cli;

/// <summary>The `skills` command group: `install` writes the `isoplane` agent skill for a project's Isoplane packages.</summary>
internal static class SkillsCommands
{
    /// <summary>
    /// Runs <c>isoplane skills install</c>. <paramref name="confirm"/> asks the person at the terminal a
    /// yes-or-no question, and is null when nobody is there to answer.
    /// </summary>
    internal static async Task<int> InstallAsync(ParseResult parsed, TextWriter output, Func<string, Task<bool>>? confirm, CancellationToken cancellation)
    {
        var directory = Path.GetFullPath(parsed.GetValue<string>("--project") ?? Environment.CurrentDirectory);
        if (!Directory.Exists(directory)) throw new ArgumentException($"Project directory does not exist: {directory}");
        var project = ManagedBundle.ProjectFile(directory);
        var named = parsed.GetValue<string>("--skills-path") is { } path ? Path.GetFullPath(Path.Combine(path, AgentSkill.Name)) : null;
        var selected = AgentSkill.Directories.Where(candidate => parsed.GetValue<bool>(candidate.Option)).ToArray();
        var options = string.Join(", ", AgentSkill.Directories.Select(candidate => candidate.Option)) + " or --skills-path";
        var ask = named is null && selected.Length == 0;
        if (ask && confirm is null)
            throw new ArgumentException($"Choose where to install the skill with {options}; standard input is not a terminal, so the command cannot ask.");
        // Where each skills directory gets the skill is settled before the restore, which takes far longer.
        var destinations = new List<string>();
        foreach (var candidate in ask ? AgentSkill.Directories : selected)
        {
            if (!ask || await confirm!($"Install the skill into {candidate.Path}, for {candidate.Readers}?"))
                destinations.Add(SkillInstallation.Destination(directory, candidate));
        }
        if (named is not null) destinations.Add(named);
        if (destinations.Count == 0)
            throw new InvalidOperationException($"No skills directory was chosen, so nothing was written. Answer y to a question, or pass {options}.");
        var sources = await SkillSources.EvaluateAsync(project, cancellation);
        var installation = new SkillInstallation(destinations, AgentSkill.Assemble(sources.BaseFile, sources.Sections));
        if (!parsed.GetValue<bool>("--force") && installation.Existing() is { Count: > 0 } existing)
            throw new InvalidOperationException(Refusal(existing));
        installation.Write();
        output.WriteLine($"Installed the {AgentSkill.Name} skill for Isoplane.Sdk {sources.SdkVersion}:");
        foreach (var written in installation.SkillDirectories) output.WriteLine($"  {written}");
        output.WriteLine("Commit these directories, so that an agent working from a clone has the skill.");
        return 0;
    }

    private static string Refusal(IReadOnlyList<string> existing) =>
        (existing.Count == 1 ? "A file the skill would create already exists" : $"{existing.Count} files the skill would create already exist") +
        $", so nothing was written:{Environment.NewLine}{string.Join(Environment.NewLine, existing.Select(file => "  " + file))}{Environment.NewLine}" +
        (existing.Count == 1 ? "Pass --force to overwrite it." : "Pass --force to overwrite them.");
}

/// <summary>The skill's <c>SKILL.md</c>, <paramref name="Skill"/>, as it would be written into each of the <paramref name="SkillDirectories"/>.</summary>
internal sealed record SkillInstallation(IReadOnlyList<string> SkillDirectories, byte[] Skill)
{
    private IEnumerable<string> Targets => SkillDirectories.Select(directory => Path.GetFullPath(Path.Combine(directory, AgentSkill.FileName)));

    /// <summary>The files the installation would create that are already there, whatever they hold.</summary>
    internal IReadOnlyList<string> Existing() => Targets.Where(File.Exists).ToArray();

    /// <summary>Writes the skill into every directory, replacing a <c>SKILL.md</c> there and leaving every other file as it is.</summary>
    internal void Write()
    {
        foreach (var path in Targets)
        {
            Directory.CreateDirectory(Path.GetDirectoryName(path)!);
            File.WriteAllBytes(path, Skill);
        }
    }

    /// <summary>
    /// The directory the skill occupies in the skills directory <paramref name="skills"/> for the project
    /// in <paramref name="project"/>: where that skills directory already holds it, looking from the
    /// project directory up to the repository's root, since a coding agent started in the project loads
    /// skills from each of those; else under <see cref="FindRoot"/>. Fails, naming the copies, when more
    /// than one of those directories holds it.
    /// </summary>
    internal static string Destination(string project, SkillsDirectory skills)
    {
        string Under(string level) => Path.GetFullPath(Path.Combine(level, skills.Path, AgentSkill.Name));
        var top = Ancestors(project).FirstOrDefault(HasGitEntry)?.FullName ?? FindRoot(project);
        var copies = Ancestors(project).Select(level => level.FullName).TakeWhile(level => level.Length >= top.Length)
            .Select(Under).Where(Directory.Exists).ToArray();
        return copies.Length switch
        {
            0 => Under(FindRoot(project)),
            1 => copies[0],
            _ => throw new InvalidOperationException(
                $"The {AgentSkill.Name} skill is in more than one {skills.Path} directory at or above the project, so the command cannot tell which to write, and nothing was written:{Environment.NewLine}" +
                string.Join(Environment.NewLine, copies.Select(copy => "  " + copy)) +
                $"{Environment.NewLine}Remove the copies you do not want, then run the command again."),
        };
    }

    private static IEnumerable<DirectoryInfo> Ancestors(string project)
    {
        for (var directory = new DirectoryInfo(project); directory is not null; directory = directory.Parent) yield return directory;
    }

    private static bool HasGitEntry(DirectoryInfo directory) =>
        Directory.Exists(Path.Combine(directory.FullName, ".git")) || File.Exists(Path.Combine(directory.FullName, ".git"));

    /// <summary>
    /// The nearest directory at or above the project holding a <c>.git</c> entry or a solution file, where
    /// a coding agent opens the code; else the project directory. The nearest, not the first repository:
    /// a <c>.git</c> far above, such as a home directory's dotfiles, must not claim a project below it.
    /// </summary>
    private static string FindRoot(string project) =>
        Ancestors(project).FirstOrDefault(directory => HasGitEntry(directory) || directory.EnumerateFiles("*.sln").Any() || directory.EnumerateFiles("*.slnx").Any())?.FullName
            ?? project;
}
