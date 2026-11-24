using System.Text.Json;

namespace Isoplane.Cli;

/// <summary>
/// The skill content a project's Isoplane packages carry: the base <c>SKILL.md</c>, each other package's
/// section, and the SDK version they belong to. Each package declares its own as MSBuild items, so
/// evaluating the restored project gives the content for exactly the packages it references.
/// </summary>
internal sealed record SkillSources(string BaseFile, IReadOnlyList<string> Sections, string SdkVersion)
{
    private const string BaseItem = "IsoplaneAgentSkillBase";
    private const string SectionItem = "IsoplaneAgentSkillPart";

    /// <summary>
    /// Restores <paramref name="project"/> as a build does, then reads the skill content its packages
    /// declare. An evaluation alone does not restore, so the two run one after the other. MSBuild
    /// reports a failed restore or evaluation itself.
    /// </summary>
    internal static async Task<SkillSources> EvaluateAsync(string project, CancellationToken cancellation)
    {
        var directory = Path.GetDirectoryName(project)!;
        if (await Processes.RunAsync("dotnet", ["restore", project, "-nologo", "--verbosity", "quiet"], directory, cancellation, redirectOutput: true) != 0)
            throw new InvalidOperationException("Could not restore the project; fix the restore errors above.");
        using var evaluation = Processes.Start("dotnet", ["msbuild", project, "-nologo",
            "-getProperty:IsoplaneSdkVersion", $"-getItem:{BaseItem},{SectionItem}"], directory, captureOutput: true);
        try
        {
            var output = await evaluation.StandardOutput.ReadToEndAsync(cancellation);
            await evaluation.WaitForExitAsync(cancellation);
            if (evaluation.ExitCode != 0)
                throw new InvalidOperationException("Could not evaluate the project; fix the project diagnostics above.");
            using var reported = JsonDocument.Parse(output);
            return Read(reported.RootElement, project);
        }
        finally
        {
            await Processes.StopAsync(evaluation);
        }
    }

    /// <summary>Reads what <c>dotnet msbuild -getProperty -getItem</c> reported for <paramref name="project"/>.</summary>
    private static SkillSources Read(JsonElement evaluation, string project)
    {
        var items = evaluation.GetProperty("Items");
        var name = Path.GetFileName(project);
        var bases = Paths(items.GetProperty(BaseItem));
        if (bases.Length == 0)
            throw new InvalidOperationException($"{name} references no Isoplane package that carries the agent skill. Reference Isoplane.Sdk or Isoplane.AspNetCore at a version that carries it, then run this command again.");
        return new(bases[0], Paths(items.GetProperty(SectionItem)),
            evaluation.GetProperty("Properties").GetProperty("IsoplaneSdkVersion").GetString() is { Length: > 0 } version ? version
                : throw new InvalidOperationException($"{name} evaluates to no IsoplaneSdkVersion, which a project gets from the Isoplane SDK its Sdk attribute names."));
    }

    private static string[] Paths(JsonElement items) =>
        items.EnumerateArray().Select(item => item.GetProperty("FullPath").GetString()!).ToArray();
}
