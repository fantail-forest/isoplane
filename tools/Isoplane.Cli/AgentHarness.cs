using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace Isoplane.Cli;

/// <summary>
/// The coding agents `isoplane agent init` can register the agent server with, each by the
/// project-level file its documentation names and the shape that file takes. An entry is written
/// into an existing file beside whatever else it holds.
/// </summary>
internal sealed record AgentHarness(string Name, string File, AgentHarness.Layout Shape)
{
    /// <summary>How a harness's file names its servers.</summary>
    internal enum Layout
    {
        /// <summary>A `mcpServers` object of `command` and `args`, as Claude Code, Pi and Antigravity read.</summary>
        McpServers,
        /// <summary>`mcpServers` with `"type": "stdio"`, as Cursor documents.</summary>
        McpServersStdio,
        /// <summary>`mcpServers` with `"type": "local"` and `tools`, as the Copilot CLI documents.</summary>
        McpServersLocal,
        /// <summary>A `servers` object with `"type": "stdio"`, as VS Code reads.</summary>
        Servers,
        /// <summary>An `mcp` object of local servers whose `command` is one array, as OpenCode reads.</summary>
        OpenCode,
        /// <summary>A `[mcp_servers.isoplane]` TOML table, as Codex reads.</summary>
        CodexToml,
    }

    internal const string ServerName = "isoplane";
    private static readonly string[] Arguments = ["agent", "mcp"];
    private static readonly JsonSerializerOptions Formatting = new() { WriteIndented = true };
    private static readonly JsonDocumentOptions Lenient = new() { CommentHandling = JsonCommentHandling.Skip, AllowTrailingCommas = true };

    internal static readonly AgentHarness[] All =
    [
        new("claude-code", ".mcp.json", Layout.McpServers),
        new("codex", ".codex/config.toml", Layout.CodexToml),
        new("cursor", ".cursor/mcp.json", Layout.McpServersStdio),
        new("opencode", "opencode.json", Layout.OpenCode),
        new("pi", ".pi/mcp.json", Layout.McpServers),
        new("copilot", ".github/mcp.json", Layout.McpServersLocal),
        new("vscode", ".vscode/mcp.json", Layout.Servers),
        new("antigravity", ".agents/mcp_config.json", Layout.McpServers),
        new("gemini", ".gemini/settings.json", Layout.McpServers),
    ];

    internal static string[] Names => All.Select(harness => harness.Name).ToArray();

    internal static AgentHarness Named(string name) => All.FirstOrDefault(harness => harness.Name == name)
        ?? throw new ArgumentException($"Unknown harness '{name}'. Choose from {string.Join(", ", Names)}.");

    /// <summary>Writes the entry into the harness's file under <paramref name="directory"/>, and returns that file's path.</summary>
    internal string Register(string directory)
    {
        var path = Path.Combine(directory, File);
        if (Shape == Layout.OpenCode && !System.IO.File.Exists(path) && System.IO.File.Exists(Path.ChangeExtension(path, ".jsonc")))
            path = Path.ChangeExtension(path, ".jsonc");
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        if (Shape == Layout.CodexToml) System.IO.File.WriteAllText(path, Toml(System.IO.File.Exists(path) ? System.IO.File.ReadAllText(path) : ""), new UTF8Encoding(false));
        else System.IO.File.WriteAllText(path, Json(System.IO.File.Exists(path) ? System.IO.File.ReadAllText(path) : null) + "\n", new UTF8Encoding(false));
        return path;
    }

    /// <summary>The file with the entry set, keeping every other member; comments cannot be kept.</summary>
    internal string Json(string? existing)
    {
        var root = existing is null || existing.Trim().Length == 0 ? new JsonObject()
            : JsonNode.Parse(existing, documentOptions: Lenient) as JsonObject
                ?? throw new InvalidOperationException($"{File} does not hold a JSON object.");
        var collection = Shape switch { Layout.Servers => "servers", Layout.OpenCode => "mcp", _ => "mcpServers" };
        var servers = root[collection] as JsonObject ?? new JsonObject();
        root[collection] = servers;
        servers[ServerName] = Entry();
        return root.ToJsonString(Formatting);
    }

    private JsonObject Entry()
    {
        var args = new JsonArray(Arguments.Select(argument => (JsonNode)argument).ToArray());
        return Shape switch
        {
            Layout.McpServers => new JsonObject { ["command"] = ServerName, ["args"] = args },
            Layout.McpServersStdio or Layout.Servers => new JsonObject { ["type"] = "stdio", ["command"] = ServerName, ["args"] = args },
            Layout.McpServersLocal => new JsonObject { ["type"] = "local", ["command"] = ServerName, ["args"] = args, ["tools"] = new JsonArray("*") },
            Layout.OpenCode => new JsonObject { ["type"] = "local", ["command"] = new JsonArray([(JsonNode)ServerName, .. Arguments.Select(argument => (JsonNode)argument)]), ["enabled"] = true },
            _ => throw new InvalidOperationException($"{Shape} is not a JSON layout."),
        };
    }

    /// <summary>The TOML file with the server's table replaced or appended; the rest of the file is kept as written.</summary>
    internal static string Toml(string existing)
    {
        var table = $"[mcp_servers.{ServerName}]\ncommand = \"{ServerName}\"\nargs = [{string.Join(", ", Arguments.Select(argument => $"\"{argument}\""))}]\n";
        var lines = existing.Replace("\r\n", "\n").Split('\n').ToList();
        var start = lines.FindIndex(line => line.Trim() == $"[mcp_servers.{ServerName}]");
        if (start < 0)
        {
            var text = existing.TrimEnd();
            return text.Length == 0 ? table : text + "\n\n" + table;
        }
        var end = lines.FindIndex(start + 1, line => line.TrimStart().StartsWith('['));
        var last = end < 0;
        if (last) end = lines.Count;
        lines.RemoveRange(start, end - start);
        lines.Insert(start, last ? table.TrimEnd() : table);
        return string.Join("\n", lines).TrimEnd() + "\n";
    }
}
