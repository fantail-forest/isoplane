using Isoplane.Cli.Tests;
using System.Text.Json;
using System.Text.Json.Nodes;
using Isoplane.Native;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class NativeSessionTests
{
    [Fact]
    public void preparation_keeps_compiled_artifacts_inside_the_project()
    {
        using var directory = new SessionDirectory();
        LocalPreparation.Prepare(ModuleValidationTests.Module, directory.Options);
        var artifacts = Directory.GetFiles(Path.Combine(directory.ProjectDirectory, ".isoplane"), "*.cwasm", SearchOption.AllDirectories);
        Assert.True(artifacts.Length > 0, "Preparing a project must store its compiled artifacts under that project.");
    }

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

    [Fact]
    public async Task a_prepared_application_starts_without_native_compilation()
    {
        using var directory = new SessionDirectory();
        var preparation = LocalPreparation.Prepare(ModuleValidationTests.Module, directory.Options);
        Assert.True(preparation["compiled"]!.GetValue<bool>(),
            "The first preparation must compile the application's native artifact.");
        var repeated = LocalPreparation.Prepare(ModuleValidationTests.Module, directory.Options);
        Assert.False(repeated["compiled"]!.GetValue<bool>(),
            "An unchanged build must reuse its native artifact.");
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        var ready = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var compilation = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        await using var session = new LocalSession(ModuleValidationTests.Module, directory.Options, message =>
        {
            if (message["event"]?.GetValue<string>() == "ready") ready.TrySetResult();
            if (message["fields"] is JsonObject fields && fields["compiled"] is JsonNode compiled)
                compilation.TrySetResult(compiled.GetValue<bool>());
        });
        await ready.Task.WaitAsync(deadline.Token);
        Assert.False(await compilation.Task.WaitAsync(deadline.Token),
            "The first listener startup after preparation must load existing native code.");
    }

    [Fact]
    public void changed_module_bytes_require_new_native_compilation()
    {
        using var directory = new SessionDirectory();
        LocalPreparation.Prepare(ModuleValidationTests.Module, directory.Options);
        byte[] changed = [.. ModuleValidationTests.Module, 0, 2, 1, (byte)'x'];
        var result = LocalPreparation.Prepare(changed, directory.Options);
        Assert.True(result["compiled"]!.GetValue<bool>(),
            "A different portable module must never reuse the preceding application's native artifact.");
    }

    [Fact]
    public void changed_runtime_identity_requires_new_native_compilation()
    {
        using var directory = new SessionDirectory();
        LocalPreparation.Prepare(ModuleValidationTests.Module, directory.Options);
        var options = JsonNode.Parse(directory.Options)!;
        var runtime = Path.Combine(options["compilation_directory"]!.GetValue<string>(), "runtime-identity");
        File.WriteAllText(runtime, "changed runtime build");
        options["runtime_library"] = runtime;
        var result = LocalPreparation.Prepare(ModuleValidationTests.Module, JsonSerializer.SerializeToUtf8Bytes(options));
        Assert.True(result["compiled"]!.GetValue<bool>(),
            "A changed native runtime must never load compilation from the preceding runtime build.");
    }

    [Fact]
    public void relative_compiler_storage_is_rejected()
    {
        using var directory = new SessionDirectory();
        var options = JsonNode.Parse(directory.Options)!;
        options["compilation_directory"] = "relative-cache";
        var failure = Assert.Throws<InvalidOperationException>(() =>
            LocalPreparation.Prepare(ModuleValidationTests.Module, JsonSerializer.SerializeToUtf8Bytes(options)));
        Assert.True(failure.Message.Contains("compiler storage must be absolute", StringComparison.Ordinal),
            "Compiler storage must not resolve relative to an application checkout.");
    }

    [Theory]
    [InlineData("--port", "0")]
    [InlineData("--guest-runtime", "mono")]
    [InlineData("--debug")]
    [InlineData("--shutdown-on-stdin-close")]
    [InlineData("--no-build")]
    [InlineData("--no-watch")]
    public void preparation_refuses_execution_only_options(params string[] arguments)
    {
        var failure = Assert.Throws<ArgumentException>(() => DevelopmentOptions.Parse(arguments, preparing: true));
        Assert.True(failure.Message.Contains(arguments[0], StringComparison.Ordinal),
            "Build preparation must reject execution controls instead of ignoring them.");
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
        internal string ProjectDirectory => directory.FullName;
        internal byte[] Options { get; }

        internal SessionDirectory()
        {
            File.WriteAllText(Path.Combine(directory.FullName, "isoplane.toml"), "name='session'\nmodule='app.wasm'\n");
            var options = JsonNode.Parse(new LocalProject(directory.FullName).SessionOptions(0, []))!;
            Options = JsonSerializer.SerializeToUtf8Bytes(options);
        }

        public void Dispose() => directory.Delete(recursive: true);
    }
}
