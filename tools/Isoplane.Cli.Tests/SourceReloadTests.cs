using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class SourceReloadTests
{
    [Fact]
    public async Task runtime_exit_cancels_discovery_and_waits_for_its_cleanup()
    {
        var directory = Directory.CreateTempSubdirectory("isoplane-source-reload-");
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        using var lifetime = new CancellationTokenSource();
        var entered = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var cleaning = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var release = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var finished = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var exited = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var inputs = new SourceInputs([Path.Combine(directory.FullName, "Application.cs")], []);
        async Task<SourceInputs> Discover(CancellationToken cancellation)
        {
            entered.SetResult();
            try
            {
                await Task.Delay(Timeout.InfiniteTimeSpan, cancellation);
                return inputs;
            }
            finally
            {
                cleaning.SetResult();
                await release.Task;
                finished.SetResult();
            }
        }
        try
        {
            using var watcher = new OverflowWatcher(directory.FullName);
            using var changes = new SourceChanges(inputs, Discover, _ => watcher);
            watcher.EnableRaisingEvents = false;
            var running = SourceReload.RunAsync(changes, exited.Task,
                _ => throw new InvalidOperationException("An exited runtime must not rebuild."), lifetime.Token);
            watcher.Overflow();
            await entered.Task.WaitAsync(deadline.Token);
            exited.SetResult();
            var first = await Task.WhenAny(running, cleaning.Task).WaitAsync(deadline.Token);
            Assert.Same(cleaning.Task, first);
            Assert.False(running.IsCompleted, "Reload shutdown must wait for discovery's process cleanup.");
            release.SetResult();
            await running.WaitAsync(deadline.Token);
        }
        finally
        {
            lifetime.Cancel();
            release.TrySetResult();
            if (entered.Task.IsCompleted) await finished.Task.WaitAsync(deadline.Token);
            directory.Delete(recursive: true);
        }
    }

    private sealed class OverflowWatcher(string directory) : FileSystemWatcher(directory)
    {
        internal void Overflow() => OnError(new ErrorEventArgs(new InternalBufferOverflowException()));
    }
}
