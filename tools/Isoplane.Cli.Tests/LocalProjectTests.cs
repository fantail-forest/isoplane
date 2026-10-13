using System.Text.Json;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class LocalProjectTests : IDisposable
{
    private readonly DirectoryInfo directory = Directory.CreateTempSubdirectory("isoplane-local-project-");
    private string Project(string parent) => Path.Combine(directory.FullName, parent, "App");
    private string Target => Path.Combine(directory.FullName, "target");

    public void Dispose() => directory.Delete(recursive: true);

    private static string Option(LocalProject project, string name)
    {
        var settings = new DevelopmentSettings("App", 0, [], LimitOverrides.None, null, Path.Combine(Path.GetTempPath(), "obj"), Path.Combine(Path.GetTempPath(), "runtimes"));
        using var options = JsonDocument.Parse(project.SessionOptions(settings));
        return options.RootElement.GetProperty(name).GetString()!;
    }

    [Fact]
    public void a_project_keeps_its_files_in_its_own_directory_and_its_scratch_under_msbuild_intermediates()
    {
        var project = new LocalProject(Project("one"), null);
        var root = Path.Combine(Project("one"), ".isoplane");
        Assert.Equal(Path.Combine(root, "package", "application.tar"), project.Package);
        Assert.Equal(Path.Combine(root, "local", "trust"), project.Trust);
        Assert.StartsWith(Path.Combine(root, "local") + Path.DirectorySeparatorChar, Option(project, "state_directory"), StringComparison.Ordinal);
        Assert.StartsWith(Path.Combine(root, "debug") + Path.DirectorySeparatorChar, project.Scratch("debug"), StringComparison.Ordinal);
        Assert.Equal(Path.Combine(Path.GetTempPath(), "obj", "isoplane"), Option(project, "compilation_directory"));
    }

    [Fact]
    public void a_target_directory_takes_every_file_of_the_project_into_a_folder_of_its_own()
    {
        var project = new LocalProject(Project("one"), Target);
        var folder = Path.GetDirectoryName(Path.GetDirectoryName(project.Package))!;
        Assert.Equal(Target, Path.GetDirectoryName(folder));
        Assert.StartsWith("App-", Path.GetFileName(folder), StringComparison.Ordinal);
        foreach (var path in new[] { project.Trust, project.Scratch("bundle"), project.Scratch("debug"), Option(project, "state_directory"), Option(project, "compilation_directory") })
            Assert.StartsWith(folder + Path.DirectorySeparatorChar, path, StringComparison.Ordinal);
    }

    [Fact]
    public void projects_with_the_same_folder_name_do_not_share_a_target_folder()
    {
        Assert.NotEqual(new LocalProject(Project("one"), Target).Package, new LocalProject(Project("two"), Target).Package);
        Assert.Equal(new LocalProject(Project("one"), Target).Package, new LocalProject(Project("one"), Target).Package);
    }

    [Fact]
    public void a_target_directory_inside_the_project_is_refused()
    {
        var project = new LocalProject(Project("one"), Path.Combine(Project("one"), "output"));
        var error = Assert.Throws<InvalidOperationException>(() => project.Package);
        Assert.Contains("ISOPLANE_TARGET_DIR must be outside the project", error.Message, StringComparison.Ordinal);
    }

    [Fact]
    public void the_cache_directory_is_the_named_one_then_the_home_directorys()
    {
        var named = Path.Combine(directory.FullName, "cache");
        var home = Path.Combine(directory.FullName, "home");
        Assert.Equal(named, CliPaths.CacheFor(named, home));
        Assert.Equal(Path.Combine(home, "cache"), CliPaths.CacheFor(null, home));
        Assert.True(Path.IsPathFullyQualified(CliPaths.CacheFor(null, null)), "Without either variable the cache is the platform's per-user location.");
        var error = Assert.Throws<InvalidOperationException>(() => CliPaths.CacheFor("relative/cache", home));
        Assert.Contains("ISOPLANE_CACHE_DIR must be an absolute path", error.Message, StringComparison.Ordinal);
    }
}
