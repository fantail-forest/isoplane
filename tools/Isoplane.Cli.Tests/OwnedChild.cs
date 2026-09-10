using System.IO.Pipes;

namespace Wisp.Cli.Tests;

internal static class OwnedChild
{
    internal static async Task<int> Main(string[] arguments)
    {
        if (arguments is not ["--owned-child", var pipeName])
            return 1;
        using var pipe = new NamedPipeClientStream(".", pipeName, PipeDirection.Out, PipeOptions.Asynchronous);
        await pipe.ConnectAsync();
        using var writer = new StreamWriter(pipe) { AutoFlush = true };
        await writer.WriteLineAsync(Environment.ProcessId.ToString());
        await Task.Delay(Timeout.Infinite);
        return 0;
    }
}
