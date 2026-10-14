using System.Text.Json;
using Isoplane.Native;

namespace Isoplane.Cli.Tests;

internal static class PreparedDebugSession
{
    internal static async Task<int> RunAsync(string project, string package, string module, Isoplane.Build.DebugProject? source = null)
    {
        using var cancellation = new CommandCancellation();
        var session = Path.Combine(project, "session");
        Directory.CreateDirectory(session);
        try
        {
            await using var relay = new DebugRelay(Path.GetFullPath(package), cancellation.Token, source);
            Console.WriteLine(JsonSerializer.Serialize(new { @event = "debugger_ready", endpoint = relay.AdapterEndpoint.ToString(), package }));
            await using var host = new LocalSession(await File.ReadAllBytesAsync(module),
                new LocalProject(project).SessionOptions(new DevelopmentSettings(Path.GetFileName(project), 0, [], LimitOverrides.None, null,
                        Path.Combine(session, "obj"), CliPaths.RuntimeCache),
                    relay.Endpoint.ToString(), session, package),
                message => LocalEvents.Write(message, json: true));
            await DebugDevelopment.RunHostAsync(host, relay, cancellation.Token);
            return 0;
        }
        finally { Directory.Delete(session, recursive: true); }
    }
}
