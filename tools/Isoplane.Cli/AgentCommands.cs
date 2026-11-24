using System.CommandLine;

namespace Isoplane.Cli;

/// <summary>The `agent init` command, which registers the agent server with coding agents.</summary>
internal static class AgentCommands
{
    internal static int Init(ParseResult parsed)
    {
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
