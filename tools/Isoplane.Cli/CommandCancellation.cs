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
