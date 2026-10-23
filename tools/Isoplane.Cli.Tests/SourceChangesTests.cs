using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class SourceChangesTests
{
    [Fact]
    public async Task watcher_failures_identify_the_source_directory_and_underlying_error()
    {
        var directory = Directory.CreateTempSubdirectory("isoplane-source-changes-");
        try
        {
            using var watcher = new FailingWatcher(directory.FullName);
            using var changes = Watch(watcher);
            var failure = new IOException("watch service unavailable");
            watcher.Fail(failure);
            using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(10));
            var error = await Assert.ThrowsAsync<IOException>(() => changes.WaitAsync(deadline.Token));
            Assert.True(error.Message.Contains(directory.FullName) && error.Message.Contains(failure.Message),
                "Source watcher diagnostics must name the affected directory and the underlying failure.");
            Assert.True(ReferenceEquals(error.InnerException, failure),
                "Source watcher diagnostics must preserve the original exception.");
        }
        finally { directory.Delete(recursive: true); }
    }

    private sealed class FailingWatcher(string directory) : FileSystemWatcher(directory)
    {
        internal void Fail(Exception error) => OnError(new ErrorEventArgs(error));
        internal void Change(string file) => OnChanged(new FileSystemEventArgs(WatcherChangeTypes.Changed, Path, file));
        internal void Create(string file) => OnCreated(new FileSystemEventArgs(WatcherChangeTypes.Created, Path, file));
    }

    [Fact]
    public async Task notification_overflow_recovers_a_missed_edit_and_keeps_watching()
    {
        var directory = Directory.CreateTempSubdirectory("isoplane-source-changes-");
        try
        {
            var source = Path.Combine(directory.FullName, "Application.cs");
            File.WriteAllText(source, "initial");
            using var watcher = new FailingWatcher(directory.FullName);
            using var changes = Watch(watcher);
            watcher.EnableRaisingEvents = false;
            File.WriteAllText(source, "updated");
            watcher.Fail(new InternalBufferOverflowException());
            using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(10));
            await changes.WaitAsync(deadline.Token);
            File.WriteAllText(source, "another edit");
            watcher.Change("Application.cs");
            await changes.WaitAsync(deadline.Token);
        }
        finally { directory.Delete(recursive: true); }
    }

    [Theory]
    [InlineData("bin/generated.dll")]
    [InlineData("obj/build/generated.cs")]
    [InlineData(".isoplane/publish/output.wasm")]
    public async Task build_output_overflow_does_not_request_another_build(string artifact)
    {
        var directory = Directory.CreateTempSubdirectory("isoplane-source-changes-");
        try
        {
            using var watcher = new FailingWatcher(directory.FullName);
            using var changes = Watch(watcher);
            watcher.EnableRaisingEvents = false;
            var output = Path.Combine(directory.FullName, artifact);
            Directory.CreateDirectory(Path.GetDirectoryName(output)!);
            File.WriteAllText(output, "build output");
            watcher.Fail(new InternalBufferOverflowException());
            using var cancellation = new CancellationTokenSource();
            var waiting = changes.WaitAsync(cancellation.Token);
            Assert.False(waiting.IsCompleted,
                "An overflow containing only build outputs must keep waiting for a source change.");
            cancellation.Cancel();
            await Assert.ThrowsAnyAsync<OperationCanceledException>(() => waiting);
        }
        finally { directory.Delete(recursive: true); }
    }

    [Fact]
    public async Task overflow_preserves_an_already_observed_edit_even_if_contents_return_to_the_baseline()
    {
        var directory = Directory.CreateTempSubdirectory("isoplane-source-changes-");
        try
        {
            var source = Path.Combine(directory.FullName, "Application.cs");
            File.WriteAllText(source, "initial");
            using var watcher = new FailingWatcher(directory.FullName);
            using var changes = Watch(watcher);
            watcher.EnableRaisingEvents = false;
            File.WriteAllText(source, "an edit the in-progress build could have read");
            watcher.Change("Application.cs");
            File.WriteAllText(source, "initial");
            watcher.Fail(new InternalBufferOverflowException());
            using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(10));
            await changes.WaitAsync(deadline.Token);
        }
        finally { directory.Delete(recursive: true); }
    }

    [Theory]
    [InlineData("Dependency/Dependency.cs")]
    [InlineData("Shared/Release.cs")]
    [InlineData("Directory.Build.props")]
    [InlineData("Directory.Build.targets")]
    public async Task edits_to_evaluated_inputs_outside_the_application_request_a_rebuild(string relativePath)
    {
        using var directory = new SourceProject();
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(60));
        using var changes = await SourceChanges.CreateAsync(directory.Options, deadline.Token);
        var source = Path.GetFullPath(Path.Combine(directory.Path, relativePath));
        Assert.True(changes.IsSourceFile(source), $"The evaluated application input must be watched: {source}");
        var waiting = changes.WaitAsync(deadline.Token);
        File.AppendAllText(source, "\n");
        await waiting;
    }

    [Fact]
    public async Task discovery_uses_the_build_configuration_and_excludes_outputs_and_session_configuration()
    {
        using var directory = new SourceProject();
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(60));
        using var changes = await SourceChanges.CreateAsync(directory.Options, deadline.Token);
        Assert.True(changes.IsSourceFile(Path.Combine(directory.Path, "Shared", "Release.cs")));
        Assert.False(changes.IsSourceFile(Path.Combine(directory.Path, "Shared", "Debug.cs")));
        Assert.True(changes.IsSourceFile(Path.Combine(directory.Path, "Shared", "Hook.cs")));
        Assert.False(changes.IsSourceFile(Path.Combine(directory.Path, "artifacts", "bin", "Application", "release", "Application.dll")));
        Assert.False(changes.IsSourceFile(Path.Combine(directory.Options.Directory, ".isoplane", "bundle", "inputs.json")));
        Assert.False(changes.IsSourceFile(Path.Combine(directory.Options.Directory, "Properties", "launchSettings.json")));
    }

    [Fact]
    public async Task adding_a_source_in_a_referenced_project_refreshes_the_watched_inputs()
    {
        using var directory = new SourceProject();
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(60));
        using var changes = await SourceChanges.CreateAsync(directory.Options, deadline.Token);
        var source = Path.Combine(directory.Path, "Dependency", "Nested", "Added.cs");
        var waiting = changes.WaitAsync(deadline.Token);
        Directory.CreateDirectory(System.IO.Path.GetDirectoryName(source)!);
        File.WriteAllText(source, "public sealed class Added {}\n");
        await waiting;
        Assert.True(changes.IsSourceFile(source), $"The SDK must discover a newly globbed source: {source}");
    }

    [Fact]
    public async Task a_project_edit_refreshes_linked_sources_and_their_watchers()
    {
        using var directory = new SourceProject();
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(60));
        using var changes = await SourceChanges.CreateAsync(directory.Options, deadline.Token);
        var project = Path.Combine(directory.Options.Directory, "Application.csproj");
        var waiting = changes.WaitAsync(deadline.Token);
        var original = File.ReadAllText(project);
        File.WriteAllText(project, "<Project>");
        await waiting;
        waiting = changes.WaitAsync(deadline.Token);
        File.WriteAllText(project, original.Replace("$(Configuration).cs", "Debug.cs", StringComparison.Ordinal));
        await waiting;
        var source = Path.Combine(directory.Path, "Shared", "Debug.cs");
        Assert.True(changes.IsSourceFile(source));
        Assert.False(changes.IsSourceFile(Path.Combine(directory.Path, "Shared", "Release.cs")));
        waiting = changes.WaitAsync(deadline.Token);
        File.AppendAllText(source, "\n");
        await waiting;
    }

    private sealed class SourceProject : IDisposable
    {
        internal string Path { get; } = Directory.CreateTempSubdirectory("isoplane-source-project-").FullName;
        internal DevelopmentOptions Options => new(System.IO.Path.Combine(Path, "Application"), null, true, false, false, false, Configuration: "Release");

        internal SourceProject()
        {
            try
            {
                var fixture = Environment.GetEnvironmentVariable("ISOPLANE_TEST_SOURCE_CHANGES")
                    ?? throw new InvalidOperationException("Run task test:source-changes-fixture before the source-watching tests.");
                var assembly = System.IO.Path.Combine(fixture, "artifacts", "bin", "Application", "release", "Application.dll");
                if (!File.Exists(assembly))
                    throw new InvalidOperationException($"Missing {assembly}. Run task test:source-changes-fixture.");
                foreach (var file in Directory.EnumerateFiles(fixture, "*", SearchOption.AllDirectories))
                {
                    var target = System.IO.Path.Combine(Path, System.IO.Path.GetRelativePath(fixture, file));
                    Directory.CreateDirectory(System.IO.Path.GetDirectoryName(target)!);
                    File.Copy(file, target);
                }
            }
            catch
            {
                Dispose();
                throw;
            }
        }

        public void Dispose() => Directory.Delete(Path, recursive: true);
    }

    [Fact]
    public async Task structural_changes_in_evaluated_output_directories_do_not_repeat_discovery()
    {
        var directory = Directory.CreateTempSubdirectory("isoplane-source-changes-");
        try
        {
            using var watcher = new FailingWatcher(directory.FullName);
            var inputs = new SourceInputs([Path.Combine(directory.FullName, "Application.cs")],
                [Path.Combine(directory.FullName, "custom-outputs")]);
            var discoveries = 0;
            Task<SourceInputs> Discover(CancellationToken cancellation)
            {
                discoveries++;
                return Task.FromResult(inputs);
            }
            using var changes = new SourceChanges(inputs, Discover, _ => watcher);
            watcher.EnableRaisingEvents = false;
            watcher.Create(Path.Combine("custom-outputs", "generated.cs"));
            using var cancellation = new CancellationTokenSource();
            var waiting = changes.WaitAsync(cancellation.Token);
            Assert.False(waiting.IsCompleted);
            Assert.Equal(0, discoveries);
            cancellation.Cancel();
            await Assert.ThrowsAnyAsync<OperationCanceledException>(() => waiting);
        }
        finally { directory.Delete(recursive: true); }
    }

    private static SourceChanges Watch(FileSystemWatcher watcher)
    {
        var inputs = new SourceInputs([Path.Combine(watcher.Path, "Application.cs")],
            [Path.Combine(watcher.Path, "bin"), Path.Combine(watcher.Path, "obj"), Path.Combine(watcher.Path, ".isoplane")]);
        return new(inputs, _ => Task.FromResult(inputs), _ => watcher);
    }
}
