using System.Text.Json;
using Isoplane.Native;

namespace Isoplane.Cli.Tests;

internal static class PreparedDebugSession
{
    internal static async Task<int> RunAsync(string project, string package, string module)
    {
        using var cancellation = new CommandCancellation();
        var session = Path.Combine(project, "session");
        Directory.CreateDirectory(session);
        try
        {
            await using var relay = new DebugRelay(cancellation.Token);
            Console.WriteLine(JsonSerializer.Serialize(new { @event = "debugger_ready", endpoint = relay.AdapterEndpoint.ToString(), package }));
            await using var host = new LocalSession(await File.ReadAllBytesAsync(module),
                new LocalProject(project).SessionOptions(0, [], relay.Endpoint.ToString(), session, package),
                message => LocalEvents.Write(message, json: true));
            await DebugDevelopment.RunHostAsync(host, relay, cancellation.Token);
            return 0;
        }
        finally { Directory.Delete(session, recursive: true); }
    }
}
