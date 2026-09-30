using System.Net;
using System.Net.Sockets;
using System.Text.Json.Nodes;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class TerminalDebugLaunchTests
{
    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task Losing_the_editor_during_startup_cancels_the_application(bool reset)
    {
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(20));
        using var lifetime = new CancellationTokenSource();
        using var listener = new TcpListener(IPAddress.Loopback, 0);
        listener.Start();
        using var editor = new TcpClient();
        await editor.ConnectAsync((IPEndPoint)listener.LocalEndpoint, deadline.Token);
        using var application = await listener.AcceptTcpClientAsync(deadline.Token);
        var connected = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        await using var launch = new TerminalDebugLaunch(application, connected.Task, lifetime);
        var cancelled = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        using var registration = lifetime.Token.Register(() => cancelled.TrySetResult());
        try
        {
            if (reset) editor.Client.LingerState = new LingerOption(true, 0);
            editor.Dispose();
            await cancelled.Task.WaitAsync(deadline.Token);
            Assert.True(lifetime.IsCancellationRequested);
        }
        finally { lifetime.Cancel(); }
    }

    [Fact]
    public async Task Startup_connection_closure_after_control_handoff_does_not_stop_the_application()
    {
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(20));
        using var lifetime = new CancellationTokenSource();
        using var listener = new TcpListener(IPAddress.Loopback, 0);
        listener.Start();
        using var editor = new TcpClient();
        await editor.ConnectAsync((IPEndPoint)listener.LocalEndpoint, deadline.Token);
        using var application = await listener.AcceptTcpClientAsync(deadline.Token);
        var connected = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        await using var launch = new TerminalDebugLaunch(application, connected.Task, lifetime);
        try
        {
            var announced = launch.AnnounceAsync("127.0.0.1:1234", new JsonObject { ["app"] = "test", ["url"] = "http://127.0.0.1:5678" }, deadline.Token);
            using (var reader = new StreamReader(editor.GetStream(), leaveOpen: true))
            {
                var information = JsonNode.Parse((await reader.ReadLineAsync(deadline.Token))!);
                Assert.Equal("127.0.0.1:1234", information!["endpoint"]!.GetValue<string>());
            }
            connected.SetResult();
            editor.Dispose();
            await announced.WaitAsync(deadline.Token);
            Assert.False(lifetime.IsCancellationRequested, "the control connection takes ownership before startup discovery closes");
        }
        finally { lifetime.Cancel(); }
    }
}
