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
            using var changes = new SourceChanges(watcher);
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
            using var changes = new SourceChanges(watcher);
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
            using var changes = new SourceChanges(watcher);
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
            using var changes = new SourceChanges(watcher);
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
    [InlineData("Application.cs", true)]
    [InlineData("dist/nested/Other.cs", true)]
    [InlineData("bin/Debug/Application.dll", false)]
    [InlineData("obj/Debug/Application.pdb", false)]
    [InlineData(".isoplane/package/application.tar", false)]
    [InlineData(".isoplane/bundle/build/inputs.json", false)]
    public void publication_artifacts_do_not_trigger_source_rebuilds(string relativePath, bool expected)
    {
        var directory = Directory.CreateTempSubdirectory("isoplane-source-changes-");
        try
        {
            using var changes = new SourceChanges(directory.FullName);
            Assert.True(changes.IsSourceFile(Path.Combine(directory.FullName, relativePath)) == expected,
                $"Source edits must trigger rebuilds while publication artifacts must not: {relativePath}.");
        }
        finally
        {
            directory.Delete(recursive: true);
        }
    }
}
