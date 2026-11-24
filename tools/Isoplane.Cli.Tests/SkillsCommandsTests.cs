using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class SkillsCommandsTests : IDisposable
{
    private const string Skill = ".agents/skills/isoplane/SKILL.md";
    private const string Agents = ".agents/skills";
    private const string Claude = ".claude/skills";

    private readonly SkillFixture fixture = new();

    public void Dispose() => fixture.Dispose();

    private async Task<string> Install(Func<string, Task<bool>>? confirm, params string[] options)
    {
        using var output = new StringWriter();
        using var deadline = new CancellationTokenSource(TimeSpan.FromMinutes(2));
        Assert.Equal(0, await SkillsCommands.InstallAsync(CliCommandLine.Parse(["skills", "install", "--project", fixture.ProjectDirectory, .. options]), output, confirm, deadline.Token));
        return output.ToString();
    }

    /// <summary>The repository's <paramref name="path"/>, given with forward slashes, as the command reports it: absolute, with the platform's separators.</summary>
    private string Reported(string path) => Path.GetFullPath(Path.Combine(fixture.Repository, path));

    /// <summary>The skill as assembled for the fixture project when its packages carry <paramref name="sections"/>.</summary>
    private byte[] Assembled(params string[] sections) => AgentSkill.Assemble(fixture.Base, sections.Select(fixture.Section));

    /// <summary>Fails unless the skill in <paramref name="skillsDirectory"/> is exactly the one assembled for <paramref name="sections"/>.</summary>
    private void AssertInstalled(string skillsDirectory, params string[] sections)
    {
        AssertSameFiles(new() { ["SKILL.md"] = Assembled(sections) }, fixture.Files($"{skillsDirectory}/isoplane"));
    }

    private static void AssertSameFiles(Dictionary<string, byte[]> expected, Dictionary<string, byte[]> actual)
    {
        Assert.Equal(expected.Keys.Order(StringComparer.Ordinal), actual.Keys.Order(StringComparer.Ordinal));
        foreach (var (path, content) in expected) Assert.True(content.AsSpan().SequenceEqual(actual[path]), $"{path} differs");
    }

    [Fact]
    public async Task install_writes_the_skill_for_the_projects_packages_into_the_chosen_directory_at_the_root()
    {
        fixture.Project(["aspnetcore"]);
        var output = await Install(null, "--dot-agents");
        AssertInstalled(Agents, "aspnetcore");
        Assert.False(Directory.Exists(Path.Combine(fixture.Repository, ".claude")), "a directory that was not chosen must not be created");
        Assert.False(Directory.Exists(Path.Combine(fixture.ProjectDirectory, ".agents")), "the skill belongs at the root, not beside the project");
        Assert.Contains(fixture.Repository, output);
        Assert.Contains(Path.Combine(".agents", "skills", "isoplane"), output);
    }

    [Fact]
    public async Task both_options_write_the_same_files_into_both_directories()
    {
        fixture.Project([]);
        await Install(null, "--dot-agents", "--dot-claude");
        AssertInstalled(Agents);
        AssertInstalled(Claude);
    }

    [Fact]
    public async Task install_refuses_and_writes_nothing_when_a_file_it_would_create_exists()
    {
        fixture.Project([]);
        fixture.Write(Skill, "mine\n");
        var before = fixture.Files(".agents");
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => Install(null, "--dot-agents", "--dot-claude"));
        Assert.Contains(Reported(Skill), error.Message);
        AssertSameFiles(before, fixture.Files(".agents"));
        Assert.False(Directory.Exists(Path.Combine(fixture.Repository, ".claude")), "a refusal must write nothing, in any directory");
    }

    [Fact]
    public async Task a_refusal_names_each_existing_file_of_a_later_chosen_directory_and_writes_nothing_into_an_earlier_one()
    {
        fixture.Project([]);
        await Install(null, "--dot-claude");
        var before = fixture.Files("");
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => Install(null, "--dot-agents", "--dot-claude"));
        Assert.Contains(Reported($"{Claude}/isoplane/SKILL.md"), error.Message);
        AssertSameFiles(before, fixture.Files(""));
    }

    [Fact]
    public async Task force_overwrites_the_skills_files_and_leaves_every_other_file_as_it_was()
    {
        fixture.Project([]);
        fixture.Write(Skill, "edited\n");
        fixture.Write(".agents/skills/isoplane/notes.md", "my notes\n");
        fixture.Write(".agents/skills/isoplane/references/team-notes/notes.md", "my notes\n");
        fixture.Write(".agents/skills/mine/SKILL.md", "---\nname: mine\n---\n");
        var expected = fixture.Files(Agents);
        expected["isoplane/SKILL.md"] = Assembled();
        await Install(null, "--dot-agents", "--force");
        AssertSameFiles(expected, fixture.Files(Agents));
    }

    [Fact]
    public async Task without_a_directory_option_and_nobody_to_ask_install_fails_as_a_usage_error_and_writes_nothing()
    {
        fixture.Project([]);
        await Assert.ThrowsAsync<ArgumentException>(() => Install(null));
        Assert.False(Directory.Exists(Path.Combine(fixture.Repository, ".agents")) || Directory.Exists(Path.Combine(fixture.Repository, ".claude")));
    }

    [Fact]
    public async Task without_a_directory_option_install_asks_about_each_directory_and_writes_the_ones_accepted()
    {
        fixture.Project([]);
        var asked = new List<string>();
        await Install(question =>
        {
            asked.Add(question);
            return Task.FromResult(question.Contains(Claude));
        });
        Assert.Equal(2, asked.Count);
        Assert.Contains(asked, question => question.Contains(Agents));
        AssertInstalled(Claude);
        Assert.False(Directory.Exists(Path.Combine(fixture.Repository, ".agents")), "a directory that was declined must not be created");
    }

    [Fact]
    public async Task a_directory_option_answers_the_question_so_none_is_asked()
    {
        fixture.Project([]);
        await Install(question => throw new InvalidOperationException($"asked '{question}'"), "--dot-claude");
        AssertInstalled(Claude);
    }

    [Fact]
    public async Task a_project_whose_packages_carry_no_skill_fails_the_install_and_nothing_is_written()
    {
        fixture.Project(null);
        await Assert.ThrowsAsync<InvalidOperationException>(() => Install(null, "--dot-agents"));
        Assert.False(Directory.Exists(Path.Combine(fixture.Repository, ".agents")));
    }

    [Fact]
    public async Task install_restores_the_project_so_the_skill_holds_the_section_of_a_package_it_references()
    {
        fixture.Package("Stand.In.OpenTelemetry", "### Stand.In.OpenTelemetry\n");
        fixture.Project([], "Stand.In.OpenTelemetry");
        await Install(null, "--dot-agents");
        Assert.Equal(SkillFixture.BaseText + "\n### Stand.In.OpenTelemetry\n", File.ReadAllText(Path.Combine(fixture.Repository, Skill)));
    }

    [Fact]
    public async Task a_project_that_cannot_be_restored_fails_the_install_and_nothing_is_written()
    {
        fixture.Project([], "Stand.In.Missing");
        await Assert.ThrowsAsync<InvalidOperationException>(() => Install(null, "--dot-agents"));
        Assert.False(Directory.Exists(Path.Combine(fixture.Repository, ".agents")));
    }

    [Fact]
    public async Task install_updates_the_copy_a_skills_directory_already_holds_between_the_project_and_the_repository_root()
    {
        fixture.Project([]);
        fixture.Write("src/app/.claude/skills/isoplane/SKILL.md", "shipped with the project\n");
        await Install(null, "--dot-claude", "--force");
        AssertInstalled("src/app/.claude/skills");
        Assert.False(Directory.Exists(Path.Combine(fixture.Repository, ".claude")), "the root must not get a second copy");
    }

    [Fact]
    public async Task install_refuses_and_writes_nothing_when_a_skills_directory_holds_the_skill_at_more_than_one_level()
    {
        fixture.Project([]);
        fixture.Write(".claude/skills/isoplane/SKILL.md", "at the repository root\n");
        fixture.Write("src/app/.claude/skills/isoplane/SKILL.md", "beside the project\n");
        var before = fixture.Files("");
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => Install(null, "--dot-claude", "--force"));
        Assert.Contains(Reported(".claude/skills/isoplane"), error.Message);
        Assert.Contains(Reported("src/app/.claude/skills/isoplane"), error.Message);
        AssertSameFiles(before, fixture.Files(""));
    }

    [Fact]
    public async Task a_declined_skills_directory_holding_the_skill_at_more_than_one_level_does_not_stop_the_accepted_one()
    {
        fixture.Project([]);
        fixture.Write(".agents/skills/isoplane/SKILL.md", "at the repository root\n");
        fixture.Write("src/app/.agents/skills/isoplane/SKILL.md", "beside the project\n");
        fixture.Write("src/app/.claude/skills/isoplane/SKILL.md", "shipped with the project\n");
        var atRoot = fixture.Files(".agents");
        var besideProject = fixture.Files("src/app/.agents");
        await Install(question => Task.FromResult(question.Contains(Claude)), "--force");
        AssertInstalled("src/app/.claude/skills");
        AssertSameFiles(atRoot, fixture.Files(".agents"));
        AssertSameFiles(besideProject, fixture.Files("src/app/.agents"));
    }

    [Fact]
    public void the_skill_is_looked_for_up_to_the_repository_root_past_a_solution_and_no_further()
    {
        var claude = AgentSkill.Directories.Single(directory => directory.Path == Claude);
        fixture.Write("src/app.slnx", "<Solution />");
        fixture.Write("../.claude/skills/isoplane/SKILL.md", "above the repository\n");
        Assert.Equal(Reported("src/.claude/skills/isoplane"), SkillInstallation.Destination(fixture.ProjectDirectory, claude));
        fixture.Write(".claude/skills/isoplane/SKILL.md", "at the repository root\n");
        Assert.Equal(Reported(".claude/skills/isoplane"), SkillInstallation.Destination(fixture.ProjectDirectory, claude));
    }

    [Fact]
    public void without_a_copy_the_skill_goes_under_the_nearest_directory_above_the_project_with_a_git_entry_or_a_solution()
    {
        var agents = AgentSkill.Directories.Single(directory => directory.Path == Agents);
        Assert.Equal(Reported(".agents/skills/isoplane"), SkillInstallation.Destination(fixture.ProjectDirectory, agents));
        fixture.Write("src/app.slnx", "<Solution />");
        Assert.Equal(Reported("src/.agents/skills/isoplane"), SkillInstallation.Destination(fixture.ProjectDirectory, agents));
        // A worktree's .git is a file.
        fixture.Write("src/app/.git", "gitdir: elsewhere\n");
        Assert.Equal(Reported("src/app/.agents/skills/isoplane"), SkillInstallation.Destination(fixture.ProjectDirectory, agents));
    }

    [Fact]
    public void an_installation_reports_exactly_the_files_it_would_create_that_exist()
    {
        var installation = new SkillInstallation([Reported(".agents/skills/isoplane"), Reported(".claude/skills/isoplane")], [1]);
        Assert.Empty(installation.Existing());
        fixture.Write(".claude/skills/isoplane/SKILL.md", "same or not");
        fixture.Write(".agents/skills/isoplane/other.md", "not the skill's");
        Assert.Equal([Reported(".claude/skills/isoplane/SKILL.md")], installation.Existing());
    }

    [Fact]
    public async Task a_skills_path_receives_the_skill_where_it_points_without_a_question_or_a_search_for_the_root()
    {
        fixture.Project(["aspnetcore"]);
        fixture.Write(".agents/skills/isoplane/SKILL.md", "at the repository root\n");
        var before = fixture.Files("");
        var elsewhere = Path.Combine(fixture.Repository, "..", "staging", "skills");
        await Install(question => throw new InvalidOperationException($"asked '{question}'"), "--skills-path", elsewhere);
        Assert.Equal(Assembled("aspnetcore"), File.ReadAllBytes(Path.Combine(elsewhere, "isoplane", "SKILL.md")));
        AssertSameFiles(before, fixture.Files(""));
    }
}
