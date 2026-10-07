using Isoplane.Native;
using System.Net.Sockets;
using System.Net;
using System.Text.Json;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class DebugRelayTests
{
    /// <summary>The relay only names the package to the debugger; it never opens it.</summary>
    private static readonly string Package = Path.Combine(Path.GetTempPath(), "isoplane-test-application.tar");

    [Fact]
    public async Task The_debugger_is_told_the_package_and_each_rebuild_of_it()
    {
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(20));
        await using var relay = new DebugRelay(Package, deadline.Token);
        await relay.PackageChangedAsync(deadline.Token);
        using var control = new TcpClient();
        await control.ConnectAsync(relay.AdapterEndpoint, deadline.Token);
        using var reader = new StreamReader(control.GetStream(), leaveOpen: true);
        using (var opened = JsonDocument.Parse((await reader.ReadLineAsync(deadline.Token))!))
            Assert.Equal(Package, opened.RootElement.GetProperty("package").GetString());
        await relay.PackageChangedAsync(deadline.Token);
        using var rebuilt = JsonDocument.Parse((await reader.ReadLineAsync(deadline.Token))!);
        Assert.Equal("packageChanged", rebuilt.RootElement.GetProperty("action").GetString());
    }

    [Fact]
    public async Task A_transport_reset_terminates_the_owned_runtime()
    {
        await using var session = await RelaySession.StartAsync();
        session.Control.Client.LingerState = new LingerOption(true, 0);
        session.Control.Client.Dispose();
        await Assert.ThrowsAsync<IOException>(() => session.Owner.WaitAsync(session.Cancellation));
        Assert.True(session.Host.Completion.IsCompleted, "the runtime owner must stop its native session when debugger transport fails");
    }

    [Fact]
    public async Task Orderly_editor_eof_terminates_the_owned_runtime()
    {
        await using var session = await RelaySession.StartAsync();
        session.Control.Client.Shutdown(SocketShutdown.Send);
        await session.Relay.Completion.WaitAsync(session.Cancellation);
        await session.Owner.WaitAsync(session.Cancellation);
        Assert.True(session.Host.Completion.IsCompleted, "editor EOF must release the runtime even between requests");
    }

    [Fact]
    public async Task Successive_guests_use_the_same_relay_endpoints()
    {
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(20));
        await using var relay = new DebugRelay(Package, deadline.Token);
        using var control = new TcpClient();
        var endpoint = await ConnectControlAsync(control, relay, deadline.Token);
        for (int request = 0; request < 2; request++)
        {
            using var adapter = new TcpClient();
            using var guest = new TcpClient();
            await adapter.ConnectAsync(endpoint, deadline.Token);
            await guest.ConnectAsync(relay.Endpoint, deadline.Token);
            byte[] message = [(byte)(request + 1)];
            await guest.GetStream().WriteAsync(message, deadline.Token);
            byte[] received = new byte[1];
            await adapter.GetStream().ReadExactlyAsync(received, deadline.Token);
            Assert.True(received.SequenceEqual(message), "each fresh guest must reach the adapter through the session endpoint");
            await adapter.GetStream().WriteAsync(message, deadline.Token);
            await guest.GetStream().ReadExactlyAsync(received, deadline.Token);
            Assert.True(received.SequenceEqual(message), "adapter commands must reach the current guest");
            guest.Client.Shutdown(SocketShutdown.Send);
            int count = await adapter.GetStream().ReadAsync(received, deadline.Token);
            Assert.True(count == 0, "guest completion must end only its paired adapter transport");
        }
        Assert.False(relay.Completion.IsCompleted, "the relay must remain available for another request");
    }

    [Fact]
    public async Task Adapter_reset_while_waiting_for_a_guest_ends_the_relay()
    {
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(20));
        var relay = new DebugRelay(Package, deadline.Token);
        using var adapter = new TcpClient();
        await ConnectControlAsync(adapter, relay, deadline.Token);
        adapter.Client.LingerState = new LingerOption(true, 0);
        adapter.Client.Dispose();
        await Assert.ThrowsAsync<IOException>(() => relay.Completion.WaitAsync(deadline.Token));
        // Observe the same terminal error when releasing the relay owner.
        await Assert.ThrowsAsync<IOException>(() => relay.DisposeAsync().AsTask());
    }

    [Theory]
    [InlineData("{\"version\":3,\"action\":\"terminate\"}")]
    [InlineData("{\"version\":2,\"action\":\"unknown\"}")]
    [InlineData("not-json")]
    public async Task Invalid_control_commands_fail_the_session(string message)
    {
        await using var session = await RelaySession.StartAsync();
        using var writer = new StreamWriter(session.Control.GetStream(), leaveOpen: true) { AutoFlush = true };
        await writer.WriteLineAsync(message);
        await Assert.ThrowsAsync<IOException>(() => session.Owner.WaitAsync(session.Cancellation));
        Assert.True(session.Host.Completion.IsCompleted, "an incompatible control peer must not leave an owned host running");
    }

    private static async Task<IPEndPoint> ConnectControlAsync(TcpClient control, DebugRelay relay, CancellationToken cancellation)
    {
        await control.ConnectAsync(relay.AdapterEndpoint, cancellation);
        using var reader = new StreamReader(control.GetStream(), leaveOpen: true);
        var line = await reader.ReadLineAsync(cancellation);
        Assert.True(line is not null, "the relay must announce its guest transport endpoint");
        using var message = JsonDocument.Parse(line);
        return new IPEndPoint(IPAddress.Loopback, message.RootElement.GetProperty("port").GetInt32());
    }

    private sealed class RelaySession : IAsyncDisposable
    {
        private readonly CancellationTokenSource lifetime = new(TimeSpan.FromSeconds(20));
        internal DebugRelay Relay { get; }
        internal TcpClient Control { get; } = new();
        internal TcpClient First { get; } = new();
        internal TcpClient Second { get; } = new();
        internal Task Owner { get; }
        internal LocalSession Host { get; }
        internal CancellationToken Cancellation => lifetime.Token;
        private readonly TaskCompletionSource ready = new(TaskCreationOptions.RunContinuationsAsynchronously);
        private readonly SessionFixture fixture = new();

        private RelaySession()
        {
            Relay = new DebugRelay(Package, Cancellation);
            Host = new LocalSession(SessionFixture.Module, fixture.Options, message =>
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
                var endpoint = await ConnectControlAsync(session.Control, session.Relay, session.Cancellation);
                await session.First.ConnectAsync(session.Relay.Endpoint, session.Cancellation);
                await session.Second.ConnectAsync(endpoint, session.Cancellation);
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
            Control.Dispose();
            First.Dispose();
            Second.Dispose();
            await Host.DisposeAsync();
            fixture.Dispose();
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
