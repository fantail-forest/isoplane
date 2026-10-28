namespace Isoplane.Cli;

/// <summary>The `agent` command group: `mcp` serves the agent server; `init` registers it with coding agents.</summary>
internal static class AgentCommands
{
    internal static async Task<int> RunAsync(string[] args, CancellationToken cancellation)
    {
        if (args is ["agent", "mcp"]) return await AgentServer.RunAsync(cancellation);
        var parsed = CliCommandLine.Parse(args);
        var directory = Path.GetFullPath(parsed.GetValue<string>("--directory") ?? Environment.CurrentDirectory);
        if (!Directory.Exists(directory)) throw new ArgumentException($"Directory does not exist: {directory}");
        foreach (var name in parsed.GetValue<string[]>("--harness") ?? [])
        {
            var harness = AgentHarness.Named(name);
            var file = harness.Register(directory);
            Console.WriteLine($"{harness.Name}: {Path.GetRelativePath(directory, file)} runs `isoplane agent mcp`.");
        }
        return 0;
    }
}
