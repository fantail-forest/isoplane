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

    internal static int Report(string command, TextWriter error)
    {
        if (command is "setup" or "uninstall-sdk")
        {
            error.WriteLine($"isoplane: {command} cancelled before completion. Run isoplane {command} again to retry.");
            return 130;
        }
        return command is "bundle" or "prepare-runtime" ? 130 : 0;
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
