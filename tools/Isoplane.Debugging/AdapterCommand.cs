namespace Isoplane.Debugging;

/// <summary>Runs one editor connection to the local interpreted guest debugger.</summary>
public static class AdapterCommand
{
    /// <summary>Serves DAP on standard input/output until EOF or cancellation, closing the guest connection on exit.</summary>
    public static async Task<int> RunAsync(CancellationToken cancellation, DebugLauncher? launcher = null)
    {
        using var input = new DebuggerInput(Console.OpenStandardInput());
        using var output = Console.OpenStandardOutput();
        var original = Console.Out;
        Console.SetOut(Console.Error);
        try
        {
            await using var adapter = new GuestDebugAdapter(input, output, Console.Error, launcher);
            cancellation.ThrowIfCancellationRequested();
            adapter.Protocol.Run();
            using var stop = cancellation.Register(adapter.Stop);
            await Task.Run(adapter.Protocol.WaitForReader, CancellationToken.None);
            return adapter.Failed ? 1 : 0;
        }
        finally { Console.SetOut(original); }
    }
}
