using System.IO.Pipes;

namespace Wisp.Cli.Tests;

internal static class OwnedChild
{
    internal static async Task<int> Main(string[] arguments)
    {
        if (arguments is ["--run-process", var command])
        {
            using var cancellation = new CommandCancellation();
            try
            {
                return await Processes.RunAsync("sh", ["-c", command], Environment.CurrentDirectory,
                    cancellation.Token);
            }
            catch (OperationCanceledException) when (cancellation.Token.IsCancellationRequested) { return 0; }
        }
        if (arguments is ["--successful-doctor-probe"])
        {
            await Processes.CheckAsync("/bin/sh", ["-c", "echo tool-version; echo tool-details >&2"],
                AppContext.BaseDirectory, CancellationToken.None);
            return 0;
        }
        if (arguments is ["--read-local-secrets", var secretsId])
        {
            Console.WriteLine(System.Text.Json.JsonSerializer.Serialize(LocalSecrets.Read(secretsId)));
            return 0;
        }
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
