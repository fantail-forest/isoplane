using System.IO.Pipes;

namespace Wisp.Cli.Tests;

internal static class OwnedChild
{
    internal static async Task<int> Main(string[] arguments)
    {
        if (arguments is ["--read-local-secrets", var secretsId])
        {
            Console.WriteLine(System.Text.Json.JsonSerializer.Serialize(LocalSecrets.Read(secretsId)));
            return 0;
        }
        if (arguments is ["--nested-owner", var nestedDirectory])
            return await Processes.RunAsync("dotnet", [typeof(OwnedChild).Assembly.Location, "--supervised-tree", nestedDirectory],
                nestedDirectory, Path.Combine(AppContext.BaseDirectory, "runtime", "wisp"), CancellationToken.None, redirectOutput: true);
        if (arguments is ["--supervised-tree", var directory])
            return await Processes.RunAsync("sh", ["-c", "sleep 300 & echo $! > child.pid; wait"],
                directory, Path.Combine(AppContext.BaseDirectory, "runtime", "wisp"), CancellationToken.None, redirectOutput: true);
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
