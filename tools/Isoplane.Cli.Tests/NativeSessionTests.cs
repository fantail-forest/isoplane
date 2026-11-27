using Isoplane.Native;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class NativeSessionTests
{
    [Fact]
    public async Task a_callback_failure_stops_the_host_and_returns_to_managed_code()
    {
        using var directory = new SessionFixture();
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        var failure = new InvalidOperationException("consumer cannot receive events");
        var session = new LocalSession(SessionFixture.Module, directory.Options,
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
        using var firstDirectory = new SessionFixture();
        using var secondDirectory = new SessionFixture();
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
        using var directory = new SessionFixture();
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        using var release = new ManualResetEventSlim();
        var ready = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var reloaded = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        await using var session = new LocalSession(SessionFixture.Module, directory.Options, message =>
        {
            if (message is SessionEvent.Ready)
            {
                ready.TrySetResult();
                release.Wait(deadline.Token);
            }
            if (message is SessionEvent.Reloaded) reloaded.TrySetResult();
        });
        try
        {
            await ready.Task.WaitAsync(deadline.Token);
            session.Reload("superseded invalid publication"u8.ToArray(), directory.Options);
            session.Reload(SessionFixture.Module, directory.Options);
        }
        finally { release.Set(); }
        await reloaded.Task.WaitAsync(deadline.Token);
        Assert.False(session.Completion.IsCompleted, "A newer publication must remain reloadable while the native host is busy.");
    }

    [Fact]
    public async Task a_publication_the_host_cannot_prepare_is_reported_and_the_session_keeps_running()
    {
        using var directory = new SessionFixture();
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        var ready = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var failed = new TaskCompletionSource<string>(TaskCreationOptions.RunContinuationsAsynchronously);
        var reloaded = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        await using var session = new LocalSession(SessionFixture.Module, directory.Options, message =>
        {
            if (message is SessionEvent.Ready) ready.TrySetResult();
            if (message is SessionEvent.ReloadFailed failure) failed.TrySetResult(failure.Detail);
            if (message is SessionEvent.Reloaded) reloaded.TrySetResult();
        });
        await ready.Task.WaitAsync(deadline.Token);
        session.Reload("not a module"u8.ToArray(), directory.Options);
        if (await Task.WhenAny(failed.Task, session.Completion).WaitAsync(deadline.Token) == session.Completion) await session.Completion;
        Assert.NotEmpty(await failed.Task.WaitAsync(deadline.Token));
        session.Reload(SessionFixture.Module, directory.Options);
        if (await Task.WhenAny(reloaded.Task, session.Completion).WaitAsync(deadline.Token) == session.Completion) await session.Completion;
        await reloaded.Task.WaitAsync(deadline.Token);
    }

    [Fact]
    public async Task a_session_without_an_application_package_is_rejected()
    {
        using var directory = new SessionFixture();
        await Rejected(directory.Options with { Package = null! }, "package");
    }

    [Theory]
    [InlineData("compilation_directory", "compiler storage must be absolute")]
    [InlineData("runtime_cache_directory", "runtime cache must be absolute")]
    [InlineData("package", "the application package path must be absolute")]
    public async Task relative_session_storage_is_rejected(string property, string diagnostic)
    {
        using var directory = new SessionFixture();
        var options = property switch
        {
            "compilation_directory" => directory.Options with { CompilationDirectory = "relative-path" },
            "runtime_cache_directory" => directory.Options with { RuntimeCacheDirectory = "relative-path" },
            "package" => directory.Options with { Package = "relative-path" },
            _ => throw new ArgumentOutOfRangeException(nameof(property), property, "Not a session storage option."),
        };
        await Rejected(options, diagnostic);
    }

    [Theory]
    [InlineData("isoplane_initialize_runtime")]
    [InlineData("isoplane_initialize")]
    public async Task a_package_runtime_requires_both_initialization_stages(string export)
    {
        using var directory = new SessionFixture();
        var module = SessionFixture.Module;
        byte[] name = [(byte)export.Length, .. System.Text.Encoding.UTF8.GetBytes(export)];
        var offset = module.AsSpan().IndexOf(name);
        Assert.True(offset >= 0, "The staged session fixture must export the initializer this test removes.");
        module[offset + 1] = (byte)'x';
        await Rejected(directory.Options,
            "requires both runtime and application initialization exports", module);
    }

    [Fact]
    public async Task an_unchanged_runtime_reuses_the_preceding_sessions_native_compilation()
    {
        using var directory = new SessionFixture();
        Assert.True(await Compilation(directory, directory.Options), "The first session must compile its platform runtime.");
        Assert.False(await Compilation(directory, directory.Options), "A second session must reuse the same platform runtime compilation.");
        Assert.Single(Directory.GetFiles(directory.RuntimeCache, "*.cwasm", SearchOption.AllDirectories));
    }

    [Fact]
    public async Task a_second_project_reuses_the_runtime_another_project_compiled()
    {
        using var first = new SessionFixture();
        using var second = new SessionFixture(first.RuntimeCache);
        Assert.True(await Compilation(first, first.Options), "The first project must compile the platform runtime.");
        Assert.False(await Compilation(second, second.Options), "A project sharing the runtime cache must reuse another project's compilation.");
    }

    [Fact]
    public async Task changed_runtime_identity_requires_new_native_compilation()
    {
        using var directory = new SessionFixture();
        await Compilation(directory, directory.Options);
        var runtime = Path.Combine(directory.ProjectDirectory, "runtime-identity");
        File.WriteAllText(runtime, "changed runtime build");
        Assert.True(await Compilation(directory, directory.Options with { RuntimeLibrary = runtime }),
            "A changed native runtime must not load compilation from the preceding runtime build.");
    }

    private static async Task<bool> Compilation(SessionFixture directory, LocalSessionOptions options)
    {
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        var ready = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var compiled = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        await using var session = new LocalSession(SessionFixture.Module, options, message =>
        {
            if (message is SessionEvent.Ready) ready.TrySetResult();
            if (message is SessionEvent.Diagnostic { Text: var text } && text.Contains(" compiled=", StringComparison.Ordinal))
                compiled.TrySetResult(text.Contains(" compiled=true", StringComparison.Ordinal));
        });
        await ready.Task.WaitAsync(deadline.Token);
        Assert.True(Directory.GetFiles(directory.RuntimeCache, "*.cwasm", SearchOption.AllDirectories).Length > 0,
            "Session preparation must keep its compiled platform runtime in the runtime cache every project shares.");
        // The project keeps its prepared application layer, whose native payload is empty:
        // a layer runs the runtime's code.
        Assert.DoesNotContain(Directory.GetFiles(directory.ProjectDirectory, "*.cwasm", SearchOption.AllDirectories),
            file => new FileInfo(file).Length > 0);
        return await compiled.Task.WaitAsync(deadline.Token);
    }

    private static async Task Rejected(LocalSessionOptions options, string diagnostic, byte[]? runtime = null)
    {
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        var session = new LocalSession(runtime ?? SessionFixture.Module, options, _ => { });
        try
        {
            var error = await Assert.ThrowsAsync<InvalidOperationException>(() => session.Completion.WaitAsync(deadline.Token));
            Assert.True(error.Message.Contains(diagnostic, StringComparison.Ordinal), "The rejected session must identify its invalid options: " + error.Message);
        }
        finally { await Assert.ThrowsAsync<InvalidOperationException>(() => session.DisposeAsync().AsTask()); }
    }

    private static LocalSession Start(SessionFixture directory, TaskCompletionSource<string> ready) =>
        new(SessionFixture.Module, directory.Options, message =>
        {
            if (message is SessionEvent.Ready announced)
                ready.TrySetResult(announced.Url);
        });

}
