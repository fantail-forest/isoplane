using System.Diagnostics;
using System.IO.Pipes;
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
        Assert.True(session.Child.HasExited, "the runtime owner must reap its child when debugger transport fails");
    }

    [Fact]
    public async Task Orderly_transport_eof_does_not_terminate_the_owned_runtime()
    {
        await using var session = await RelaySession.StartAsync();
        session.First.Client.Shutdown(SocketShutdown.Send);
        await session.Relay.Completion.WaitAsync(session.Cancellation);
        Assert.False(session.Owner.IsCompleted, "orderly adapter EOF must remain distinct from a transport fault");
        Assert.False(session.Child.HasExited, "orderly EOF must not silently translate detach into runtime termination");
    }

    private sealed class RelaySession : IAsyncDisposable
    {
        private readonly CancellationTokenSource lifetime = new(TimeSpan.FromSeconds(20));
        internal DebugRelay Relay { get; }
        internal TcpClient First { get; } = new();
        private TcpClient Second { get; } = new();
        internal Task<int> Owner { get; }
        internal Process Child { get; private set; } = null!;
        internal CancellationToken Cancellation => lifetime.Token;
        private readonly NamedPipeServerStream ready;

        private RelaySession()
        {
            var pipeName = "wisp-debug-child-" + Guid.NewGuid().ToString("N");
            ready = new NamedPipeServerStream(pipeName, PipeDirection.In, 1, PipeTransmissionMode.Byte, PipeOptions.Asynchronous);
            Relay = new DebugRelay(Cancellation);
            Owner = DebugDevelopment.RunHostAsync("dotnet", [typeof(OwnedChild).Assembly.Location, "--owned-child", pipeName], AppContext.BaseDirectory, Relay, Cancellation);
        }

        internal static async Task<RelaySession> StartAsync()
        {
            var session = new RelaySession();
            try
            {
                await session.ready.WaitForConnectionAsync(session.Cancellation);
                using var reader = new StreamReader(session.ready, leaveOpen: true);
                var processId = await reader.ReadLineAsync(session.Cancellation)
                    ?? throw new InvalidOperationException("the owned runtime did not identify itself");
                session.Child = Process.GetProcessById(int.Parse(processId));
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

        /// <summary>Stops the real child and closes both peers even when an assertion fails.</summary>
        public async ValueTask DisposeAsync()
        {
            lifetime.Cancel();
            await ObserveTerminationAsync(Owner);
            await ObserveTerminationAsync(Relay.DisposeAsync().AsTask());
            First.Dispose();
            Second.Dispose();
            Child?.Dispose();
            ready.Dispose();
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
