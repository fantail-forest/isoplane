using System.Runtime.CompilerServices;

namespace Isoplane.Debugging;

/// <summary>Runs one editor connection to the local interpreted guest debugger.</summary>
public static class AdapterCommand
{
    /// <summary>
    /// Serves DAP on standard input/output until EOF or cancellation, closing the guest connection on exit.
    /// Fails with <see cref="DebugProtocolLibraryMissingException"/> before serving when the installation at
    /// <paramref name="home"/> lacks Microsoft's Debug Adapter Protocol library.
    /// </summary>
    public static Task<int> RunAsync(string home, CancellationToken cancellation, DebugLauncher? launcher = null)
    {
        DebugProtocolLibrary.Load(home);
        return ServeAsync(cancellation, launcher);
    }

    // Compiling a method that names the adapter resolves the protocol library, so the adapter stays out of the
    // method that makes the library resolvable.
    [MethodImpl(MethodImplOptions.NoInlining)]
    private static async Task<int> ServeAsync(CancellationToken cancellation, DebugLauncher? launcher)
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
