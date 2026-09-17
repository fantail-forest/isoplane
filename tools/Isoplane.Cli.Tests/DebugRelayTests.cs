using Wisp.Native;
using Isoplane.Cli.Tests;
using System.Net.Sockets;
using Xunit;

namespace Wisp.Cli.Tests;

public sealed class DebugRelayTests
{
    [Fact]
    public async Task A_transport_reset_terminates_the_owned_runtime()
    {
        await using var session = await RelaySession.StartAsync();
        session.First.Client.LingerState = new LingerOption(true, 0);
        session.First.Client.Dispose();
        await Assert.ThrowsAsync<IOException>(() => session.Owner.WaitAsync(session.Cancellation));
        Assert.True(session.Host.Completion.IsCompleted, "the runtime owner must stop its native session when debugger transport fails");
    }

    [Fact]
    public async Task Orderly_transport_eof_does_not_terminate_the_owned_runtime()
    {
        await using var session = await RelaySession.StartAsync();
        session.First.Client.Shutdown(SocketShutdown.Send);
        await session.Relay.Completion.WaitAsync(session.Cancellation);
        Assert.False(session.Owner.IsCompleted, "orderly adapter EOF must remain distinct from a transport fault");
        Assert.False(session.Host.Completion.IsCompleted, "orderly EOF must not silently translate detach into runtime termination");
    }

    private sealed class RelaySession : IAsyncDisposable
    {
        private readonly CancellationTokenSource lifetime = new(TimeSpan.FromSeconds(20));
        internal DebugRelay Relay { get; }
        internal TcpClient First { get; } = new();
        private TcpClient Second { get; } = new();
        internal Task Owner { get; }
        internal LocalSession Host { get; }
        internal CancellationToken Cancellation => lifetime.Token;
        private readonly TaskCompletionSource ready = new(TaskCreationOptions.RunContinuationsAsynchronously);
        private readonly DirectoryInfo directory = Directory.CreateTempSubdirectory("isoplane-relay-");

        private RelaySession()
        {
            File.WriteAllText(Path.Combine(directory.FullName, "wisp.toml"), "name='relay-test'\nmodule='app.wasm'\n");
            Relay = new DebugRelay(Cancellation);
            Host = new LocalSession(ModuleValidationTests.Module, new LocalProject(directory.FullName).SessionOptions(0, []), message =>
            {
                if (message["event"]?.GetValue<string>() == "ready") ready.TrySetResult();
            });
            Owner = DebugDevelopment.RunHostAsync(Host, Relay, Cancellation);
        }

        internal static async Task<RelaySession> StartAsync()
        {
            var session = new RelaySession();
            try
            {
                if (await Task.WhenAny(session.ready.Task, session.Owner).WaitAsync(session.Cancellation) == session.Owner)
                    await session.Owner;
                await session.ready.Task.WaitAsync(session.Cancellation);
                await session.First.ConnectAsync(session.Relay.Endpoint, session.Cancellation);
                await session.Second.ConnectAsync(session.Relay.Endpoint, session.Cancellation);
                await session.First.GetStream().WriteAsync(new byte[] { 42 }, session.Cancellation);
                var received = new byte[1];
                await session.Second.GetStream().ReadExactlyAsync(received, session.Cancellation);
                Assert.True(received[0] == 42, "the actual relay must forward bytes before a fault or EOF is injected");
                return session;
            }
            catch
            {
                await session.DisposeAsync();
                throw;
            }
        }

        /// <summary>Stops the native session and closes both peers even when an assertion fails.</summary>
        public async ValueTask DisposeAsync()
        {
            lifetime.Cancel();
            await ObserveTerminationAsync(Owner);
            await ObserveTerminationAsync(Relay.DisposeAsync().AsTask());
            First.Dispose();
            Second.Dispose();
            await Host.DisposeAsync();
            directory.Delete(recursive: true);
            lifetime.Dispose();
        }

        private static async Task ObserveTerminationAsync(Task operation)
        {
            try { await operation; }
            catch (Exception error) when (error is OperationCanceledException or IOException)
            {
                Console.Error.WriteLine($"test cleanup observed terminal session result: {error.Message}");
            }
        }
    }
}
