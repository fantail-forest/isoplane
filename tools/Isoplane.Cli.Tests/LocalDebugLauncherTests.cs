using System.Net;
using Isoplane.Processes;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class LocalDebugLauncherTests
{
    [Fact]
    public async Task an_announcement_without_its_endpoint_fails_the_launch()
    {
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        using var editor = new CancellationTokenSource();
        var output = new List<string>();
        var announced = new List<IPEndPoint>();
        using var application = Application("restoring packages", """{"event":"debugger_ready","module":"/sdk/runtime.wasm","package":"/app/application.tar","runtime":"mono"}""");
        var launch = LocalDebugLauncher.SuperviseAsync(application,
            endpoint => { announced.Add(endpoint); return Task.CompletedTask; }, output.Add, editor.Token);
        try
        {
            // The editor never cancels here, so only the launcher's own handling can end the launch.
            await Assert.ThrowsAsync<System.Text.Json.JsonException>(() => launch.WaitAsync(deadline.Token));
        }
        finally { await EndAsync(launch, editor); }

        Assert.Empty(announced);
        Assert.Equal(["restoring packages"], output);
    }

    [Fact]
    public async Task output_that_is_not_an_event_is_forwarded_and_the_announced_endpoint_is_reported()
    {
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        using var editor = new CancellationTokenSource();
        var output = new List<string>();
        var announced = new TaskCompletionSource<IPEndPoint>(TaskCreationOptions.RunContinuationsAsynchronously);
        using var application = Application("restoring packages", """{"restore":"done"}""",
            """{"event":"debugger_ready","endpoint":"127.0.0.1:4711","module":"/sdk/runtime.wasm","package":"/app/application.tar","runtime":"mono"}""");

        var launch = LocalDebugLauncher.SuperviseAsync(application, endpoint => { announced.TrySetResult(endpoint); return Task.CompletedTask; }, output.Add, editor.Token);
        try
        {
            if (await Task.WhenAny(announced.Task, launch).WaitAsync(deadline.Token) == launch)
            {
                await launch;
                Assert.Fail("The launch ended without reporting the debugger endpoint its application announced.");
            }
            Assert.Equal(IPEndPoint.Parse("127.0.0.1:4711"), await announced.Task.WaitAsync(deadline.Token));
            application.StandardInput.Close();
            await launch.WaitAsync(deadline.Token);
        }
        finally { await EndAsync(launch, editor); }

        Assert.Equal(["restoring packages", """{"restore":"done"}"""], output);
    }

    /// <summary>Cancels a launch as the editor would and waits for it to end, without waiting on one that ignores the cancellation.</summary>
    private static async Task EndAsync(Task launch, CancellationTokenSource editor)
    {
        editor.Cancel();
        var ended = launch.ContinueWith(_ => { }, TaskScheduler.Default);
        Assert.True(await Task.WhenAny(ended, Task.Delay(TimeSpan.FromSeconds(30))) == ended, "A cancelled launch must end.");
    }

    private static OwnedProcess Application(params string[] lines) =>
        Isoplane.Cli.Processes.Start("dotnet", [typeof(OwnedChild).Assembly.Location, "--write-then-wait-for-input-to-close", .. lines],
            Environment.CurrentDirectory, captureOutput: true, captureErrors: true);
}
