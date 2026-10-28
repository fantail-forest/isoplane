using System.Text.Json.Nodes;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class AgentHarnessTests
{
    [Theory]
    [InlineData("claude-code", "mcpServers", null)]
    [InlineData("cursor", "mcpServers", "stdio")]
    [InlineData("copilot", "mcpServers", "local")]
    [InlineData("vscode", "servers", "stdio")]
    [InlineData("pi", "mcpServers", null)]
    [InlineData("antigravity", "mcpServers", null)]
    [InlineData("gemini", "mcpServers", null)]
    public void a_json_harness_gets_the_server_under_its_collection(string name, string collection, string? type)
    {
        var entry = JsonNode.Parse(AgentHarness.Named(name).Json(null))![collection]!["isoplane"]!;
        Assert.Equal("isoplane", entry["command"]!.GetValue<string>());
        Assert.Equal(["agent", "mcp"], entry["args"]!.AsArray().Select(argument => argument!.GetValue<string>()));
        Assert.Equal(type, entry["type"]?.GetValue<string>());
    }

    [Fact]
    public void opencode_names_the_command_as_one_array()
    {
        var entry = JsonNode.Parse(AgentHarness.Named("opencode").Json(null))!["mcp"]!["isoplane"]!;
        Assert.Equal("local", entry["type"]!.GetValue<string>());
        Assert.Equal(["isoplane", "agent", "mcp"], entry["command"]!.AsArray().Select(part => part!.GetValue<string>()));
        Assert.True(entry["enabled"]!.GetValue<bool>());
    }

    [Fact]
    public void an_existing_file_keeps_its_other_members_and_servers()
    {
        const string existing = """
            {
              // the model this project uses
              "model": "fast",
              "mcpServers": { "other": { "command": "other" }, "isoplane": { "command": "stale" } },
            }
            """;
        var written = JsonNode.Parse(AgentHarness.Named("claude-code").Json(existing))!;
        Assert.Equal("fast", written["model"]!.GetValue<string>());
        Assert.Equal("other", written["mcpServers"]!["other"]!["command"]!.GetValue<string>());
        Assert.Equal("isoplane", written["mcpServers"]!["isoplane"]!["command"]!.GetValue<string>());
        Assert.Equal(written.ToJsonString(), JsonNode.Parse(AgentHarness.Named("claude-code").Json(written.ToJsonString()))!.ToJsonString());
    }

    [Fact]
    public void a_file_that_is_not_an_object_is_refused()
    {
        Assert.Throws<InvalidOperationException>(() => AgentHarness.Named("cursor").Json("[]"));
    }

    [Fact]
    public void the_codex_table_is_appended_once_and_replaced_in_place()
    {
        const string existing = "model = \"gpt\"\n\n[mcp_servers.isoplane]\ncommand = \"old\"\n\n[mcp_servers.other]\ncommand = \"other\"\n";
        var written = AgentHarness.Toml(existing);
        Assert.Equal("model = \"gpt\"\n\n[mcp_servers.isoplane]\ncommand = \"isoplane\"\nargs = [\"agent\", \"mcp\"]\n\n[mcp_servers.other]\ncommand = \"other\"\n", written);
        Assert.Equal(written, AgentHarness.Toml(written));
        Assert.Equal("[mcp_servers.isoplane]\ncommand = \"isoplane\"\nargs = [\"agent\", \"mcp\"]\n", AgentHarness.Toml(""));
        Assert.EndsWith("\n\n[mcp_servers.isoplane]\ncommand = \"isoplane\"\nargs = [\"agent\", \"mcp\"]\n", AgentHarness.Toml("model = \"gpt\"\n"));
    }

    [Fact]
    public void registering_writes_the_harness_file_under_the_directory()
    {
        var directory = Directory.CreateTempSubdirectory("isoplane-agent-init-").FullName;
        try
        {
            File.WriteAllText(Path.Combine(directory, "opencode.jsonc"), "{\"model\": \"fast\"}");
            Assert.Equal(Path.Combine(directory, "opencode.jsonc"), AgentHarness.Named("opencode").Register(directory));
            Assert.Equal(Path.Combine(directory, ".codex", "config.toml"), AgentHarness.Named("codex").Register(directory));
            Assert.Contains("[mcp_servers.isoplane]", File.ReadAllText(Path.Combine(directory, ".codex", "config.toml")));
            Assert.Equal("fast", JsonNode.Parse(File.ReadAllText(Path.Combine(directory, "opencode.jsonc")))!["model"]!.GetValue<string>());
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public void every_harness_has_a_distinct_name_and_file()
    {
        Assert.Equal(AgentHarness.All.Length, AgentHarness.All.Select(harness => harness.Name).Distinct().Count());
        Assert.Equal(AgentHarness.All.Length, AgentHarness.All.Select(harness => harness.File).Distinct().Count());
        Assert.Throws<ArgumentException>(() => AgentHarness.Named("emacs"));
    }
}
