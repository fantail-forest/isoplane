using System.Runtime.InteropServices;

namespace Isoplane.Cli;

internal sealed class CommandCancellation : IDisposable
{
    private readonly CancellationTokenSource source = new();
    private readonly PosixSignalRegistration? termination;

    internal CommandCancellation()
    {
        Console.CancelKeyPress += Cancel;
        if (!OperatingSystem.IsWindows())
            termination = PosixSignalRegistration.Create(PosixSignal.SIGTERM, context =>
            {
                context.Cancel = true;
                source.Cancel();
            });
    }

    internal CancellationToken Token => source.Token;

    /// <summary>
    /// Runs <paramref name="run"/> with a token that Ctrl+C or SIGTERM cancels. A cancelled run ends in
    /// its <see cref="OperationCanceledException"/>, which the CLI reports as exit status 130.
    /// </summary>
    internal static async Task<int> RunAsync(Func<CancellationToken, Task<int>> run)
    {
        using var cancellation = new CommandCancellation();
        return await run(cancellation.Token);
    }

    /// <summary>
    /// <see cref="RunAsync"/> for a command that Ctrl+C or SIGTERM ends as intended, such as a session or
    /// a server: a cancelled run is recorded in <paramref name="telemetry"/> as cancelled, with exit status 0.
    /// </summary>
    internal static async Task<int> RunUntilStoppedAsync(Func<CancellationToken, Task<int>> run, CommandTelemetry telemetry)
    {
        try
        {
            return await RunAsync(run);
        }
        catch (OperationCanceledException error) when (error.CancellationToken.IsCancellationRequested)
        {
            return telemetry.Cancelled(0);
        }
    }

    /// <summary>
    /// <see cref="RunAsync"/> for <paramref name="command"/>, which changes the machine: a cancelled run
    /// also tells <paramref name="error"/> that the change is incomplete and how to retry it.
    /// </summary>
    internal static async Task<int> RunOrRetryAsync(string command, Func<CancellationToken, Task<int>> run, TextWriter error)
    {
        try
        {
            return await RunAsync(run);
        }
        catch (OperationCanceledException cancelled) when (cancelled.CancellationToken.IsCancellationRequested)
        {
            error.WriteLine($"isoplane: {command} cancelled before completion. Run isoplane {command} again to retry.");
            throw;
        }
    }

    private void Cancel(object? sender, ConsoleCancelEventArgs arguments)
    {
        arguments.Cancel = true;
        source.Cancel();
    }

    public void Dispose()
    {
        Console.CancelKeyPress -= Cancel;
        termination?.Dispose();
        source.Dispose();
    }
}
