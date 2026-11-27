using System.Xml.Linq;
using Isoplane.TestSupport;

namespace Isoplane.Cli.Tests;

/// <summary>
/// A repository holding a project two levels below its root, and stand-ins for the skill files the
/// project's Isoplane packages would carry. The project declares the SDK's base and any section given
/// to <see cref="Project"/> as the SDK does, and gets any other section from a package in a feed beside
/// it, so the CLI restores and evaluates it with MSBuild as it does a project that references the real packages.
/// </summary>
internal sealed class SkillFixture : IDisposable
{
    private readonly FileSystemTestDirectory directory = new("isoplane-skill-");

    internal SkillFixture()
    {
        // The entry that stops the search for the root here, below the checkout the tests run in.
        Directory.CreateDirectory(Path.Combine(Repository, ".git"));
        Directory.CreateDirectory(ProjectDirectory);
        Directory.CreateDirectory(Path.Combine(directory.Path, "package"));
        File.WriteAllText(Base, BaseText);
        foreach (var (name, title) in new[] { ("aspnetcore", "Isoplane.AspNetCore"), ("opentelemetry", "Isoplane.OpenTelemetry") })
            File.WriteAllText(Section(name), $"### {title}\n\n- [Page](https://example.invalid/{name}.md)\n");
    }

    internal const string BaseText = "---\nname: isoplane\ndescription: Stand-in.\n---\n\n# Isoplane\n\n## Documentation\n";

    internal string Repository => Path.Combine(directory.Path, "repo");

    internal string ProjectDirectory => Path.Combine(Repository, "src", "app");

    internal string Base => Path.Combine(directory.Path, "package", "SKILL.md");

    internal string Section(string name) => Path.Combine(directory.Path, "package", name + ".md");

    /// <summary>The files below <paramref name="path"/> in the repository, by their path relative to it with forward slashes.</summary>
    internal Dictionary<string, byte[]> Files(string path)
    {
        var root = Path.Combine(Repository, path);
        return Directory.Exists(root)
            ? Directory.EnumerateFiles(root, "*", SearchOption.AllDirectories)
                .ToDictionary(file => Path.GetRelativePath(root, file).Replace(Path.DirectorySeparatorChar, '/'), File.ReadAllBytes)
            : [];
    }

    internal void Write(string path, string content)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(Path.Combine(Repository, path))!);
        File.WriteAllText(Path.Combine(Repository, path), content);
    }

    private string Feed => Directory.CreateDirectory(Path.Combine(directory.Path, "feed")).FullName;

    /// <summary>
    /// Writes the project, not restored, as one whose SDK carries the skill's base and <paramref name="sections"/>,
    /// or no skill at all when <paramref name="sections"/> is null, and which references <paramref name="packages"/>
    /// from the feed. Nothing else is a restore source.
    /// </summary>
    internal void Project(string[]? sections, params string[] packages)
    {
        // A restore writes beside the project by default; the tests compare the repository before and after an install.
        new XDocument(new XElement("Project", new XElement("PropertyGroup",
            new XElement("BaseIntermediateOutputPath", Path.Combine(directory.Path, "obj") + Path.DirectorySeparatorChar))))
            .Save(Path.Combine(ProjectDirectory, "Directory.Build.props"));
        new XDocument(new XElement("Project", new XAttribute("Sdk", "Microsoft.NET.Sdk"),
            new XElement("PropertyGroup", new XElement("TargetFramework", "net10.0"), new XElement("IsoplaneSdkVersion", "1.2.3"),
                new XElement("RestoreSources", Feed), new XElement("RestorePackagesPath", Path.Combine(directory.Path, "restored"))),
            new XElement("ItemGroup",
                sections is null ? null : new XElement("IsoplaneAgentSkillBase", new XAttribute("Include", Base)),
                (sections ?? []).Select(section => new XElement("IsoplaneAgentSkillPart", new XAttribute("Include", Section(section)))),
                packages.Select(package => new XElement("PackageReference", new XAttribute("Include", package), new XAttribute("Version", "1.0.0"))))))
            .Save(Path.Combine(ProjectDirectory, "App.csproj"));
    }

    /// <summary>Adds the package <paramref name="id"/> to the feed, carrying <paramref name="section"/> as an Isoplane package carries its section of the skill.</summary>
    internal void Package(string id, string section) => TestPackage.Write(Feed, id, "1.0.0", type: null, new()
    {
        [$"build/{id}.props"] = new XDocument(new XElement("Project", new XElement("ItemGroup",
            new XElement("IsoplaneAgentSkillPart", new XAttribute("Include", "$(MSBuildThisFileDirectory)../skill/section.md"))))).ToString(),
        ["skill/section.md"] = section,
    });

    public void Dispose() => directory.Dispose();
}
