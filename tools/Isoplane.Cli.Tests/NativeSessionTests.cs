using Isoplane.Cli.Tests;
using Wisp.Native;
using Xunit;

namespace Wisp.Cli.Tests;

public sealed class NativeSessionTests
{
    [Fact]
    public async Task a_callback_failure_stops_the_host_and_returns_to_managed_code()
    {
        using var directory = new SessionDirectory();
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        var failure = new InvalidOperationException("consumer cannot receive events");
        var session = new LocalSession(ModuleValidationTests.Module, directory.Options,
            _ => throw failure);
        try
        {
            var observed = await Assert.ThrowsAsync<InvalidOperationException>(() => session.Completion.WaitAsync(deadline.Token));
            Assert.True(ReferenceEquals(failure, observed), "A callback exception must return intact after native shutdown.");
        }
        finally
        {
            await Assert.ThrowsAsync<InvalidOperationException>(() => session.DisposeAsync().AsTask());
        }
    }

    [Fact]
    public async Task stopping_one_session_keeps_another_session_listening()
    {
        using var firstDirectory = new SessionDirectory();
        using var secondDirectory = new SessionDirectory();
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        var firstReady = new TaskCompletionSource<string>(TaskCreationOptions.RunContinuationsAsynchronously);
        var secondReady = new TaskCompletionSource<string>(TaskCreationOptions.RunContinuationsAsynchronously);
        await using var first = Start(firstDirectory, firstReady);
        await using var second = Start(secondDirectory, secondReady);
        await firstReady.Task.WaitAsync(deadline.Token);
        var address = await secondReady.Task.WaitAsync(deadline.Token);
        first.Stop();
        await first.Completion.WaitAsync(deadline.Token);
        using var socket = new System.Net.Sockets.TcpClient();
        await socket.ConnectAsync(new Uri(address).Host, new Uri(address).Port, deadline.Token);
        Assert.False(second.Completion.IsCompleted, "Stopping one native host must leave the other session's listener and lifetime intact.");
    }

    [Fact]
    public async Task newer_publications_replace_pending_reloads_without_ending_the_session()
    {
        using var directory = new SessionDirectory();
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        using var release = new ManualResetEventSlim();
        var ready = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var reloaded = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        await using var session = new LocalSession(ModuleValidationTests.Module, directory.Options, message =>
        {
            if (message["event"]?.GetValue<string>() == "ready")
            {
                ready.TrySetResult();
                release.Wait(deadline.Token);
            }
            if (message["event"]?.GetValue<string>() == "reloaded") reloaded.TrySetResult();
        });
        try
        {
            await ready.Task.WaitAsync(deadline.Token);
            session.Reload("superseded invalid publication"u8.ToArray(), directory.Options);
            session.Reload(ModuleValidationTests.Module, directory.Options);
        }
        finally { release.Set(); }
        await reloaded.Task.WaitAsync(deadline.Token);
        Assert.False(session.Completion.IsCompleted, "A newer publication must remain reloadable while the native host is busy.");
    }

    private static LocalSession Start(SessionDirectory directory, TaskCompletionSource<string> ready) =>
        new(ModuleValidationTests.Module, directory.Options, message =>
        {
            if (message["event"]?.GetValue<string>() == "ready")
                ready.TrySetResult(message["url"]!.GetValue<string>());
        });

    private sealed class SessionDirectory : IDisposable
    {
        private readonly DirectoryInfo directory = Directory.CreateTempSubdirectory("isoplane-native-session-");
        internal byte[] Options { get; }

        internal SessionDirectory()
        {
            File.WriteAllText(Path.Combine(directory.FullName, "wisp.toml"), "name='session'\nmodule='app.wasm'\n");
            Options = new LocalProject(directory.FullName).SessionOptions(0, []);
        }

        public void Dispose() => directory.Delete(recursive: true);
    }
}
