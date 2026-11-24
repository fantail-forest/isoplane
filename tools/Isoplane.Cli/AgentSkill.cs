using System.Text;

namespace Isoplane.Cli;

/// <summary>A skills directory the skill can be installed into, and the option that selects it.</summary>
/// <param name="Option">The <c>isoplane skills install</c> option that selects the directory.</param>
/// <param name="Path">The directory, relative to the root the skill is installed under.</param>
/// <param name="Readers">The coding agents that read skills from the directory, as a phrase that follows "for".</param>
internal sealed record SkillsDirectory(string Option, string Path, string Readers);

/// <summary>
/// The <c>isoplane</c> agent skill: the <c>SKILL.md</c> the SDK package carries, extended with the
/// section each other Isoplane package carries. <c>isoplane skills install</c> assembles it from the
/// packages a project references.
/// </summary>
internal static class AgentSkill
{
    internal const string Name = "isoplane";

    internal const string FileName = "SKILL.md";

    internal static readonly SkillsDirectory[] Directories =
    [
        new("--dot-agents", ".agents/skills", "agents that follow the Agent Skills convention"),
        new("--dot-claude", ".claude/skills", "Claude Code"),
    ];

    /// <summary>
    /// The skill's <c>SKILL.md</c>: <paramref name="baseFile"/>, then each of the <paramref name="sections"/>
    /// in the ordinal order of their file names, so the result does not depend on the order MSBuild
    /// imported the packages in.
    /// </summary>
    internal static byte[] Assemble(string baseFile, IEnumerable<string> sections)
    {
        var skill = new StringBuilder(Normalized(File.ReadAllText(baseFile)));
        foreach (var section in sections.OrderBy(Path.GetFileName, StringComparer.Ordinal))
            skill.Append('\n').Append(Normalized(File.ReadAllText(section)));
        return new UTF8Encoding(false).GetBytes(skill.ToString());
    }

    private static string Normalized(string text) => text.Replace("\r\n", "\n").TrimEnd() + "\n";
}
