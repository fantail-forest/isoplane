using System.Text.Json;
using System.Text.Json.Nodes;
using ModelContextProtocol.Client;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class AgentServerTests
{
    [Fact]
    public async Task the_server_lists_its_tools_and_answers_over_standard_streams()
    {
        using var deadline = new CancellationTokenSource(TimeSpan.FromMinutes(2));
        var transport = new StdioClientTransport(new StdioClientTransportOptions
        {
            Name = "isoplane",
            Command = "dotnet",
            Arguments = [typeof(CliPaths).Assembly.Location, "agent", "mcp"],
            EnvironmentVariables = new Dictionary<string, string?> { ["ISOPLANE_HOME"] = Directory.CreateTempSubdirectory("isoplane-agent-").FullName },
        });
        await using var client = await McpClient.CreateAsync(transport, cancellationToken: deadline.Token);
        var tools = await client.ListToolsAsync(cancellationToken: deadline.Token);
        Assert.Equal(AgentToolCatalogue.All.Select(tool => tool.Name).Order(), tools.Select(tool => tool.Name).Order());
        Assert.All(tools, tool => Assert.Equal(AgentToolCatalogue.All.Single(known => known.Name == tool.Name).Description, tool.Description));
        var listed = await client.CallToolAsync(AgentToolCatalogue.ListSessions, cancellationToken: deadline.Token);
        Assert.NotEqual(true, listed.IsError);
        var missing = await client.CallToolAsync(AgentToolCatalogue.SessionEvents, new Dictionary<string, object?> { ["sessionId"] = "nope" }, cancellationToken: deadline.Token);
        Assert.True(missing.IsError, "an unknown session must be a tool error the model can act on");
        Assert.Contains("list_sessions", string.Concat(missing.Content.OfType<ModelContextProtocol.Protocol.TextContentBlock>().Select(block => block.Text)));
    }

    [Fact]
    public async Task a_tool_call_is_reported_to_telemetry_as_the_command_it_does_over_mcp()
    {
        using var deadline = new CancellationTokenSource(TimeSpan.FromMinutes(2));
        var reported = new TaskCompletionSource<JsonElement>(TaskCreationOptions.RunContinuationsAsynchronously);
        var transport = new StdioClientTransport(new StdioClientTransportOptions
        {
            Name = "isoplane",
            Command = "dotnet",
            Arguments = [typeof(CliPaths).Assembly.Location, "agent", "mcp"],
            EnvironmentVariables = new Dictionary<string, string?>
            {
                ["ISOPLANE_HOME"] = Directory.CreateTempSubdirectory("isoplane-agent-").FullName,
                [TelemetryVariables.Debug.Name] = "1",
            },
            StandardErrorLines = line =>
            {
                if (line.StartsWith("isoplane telemetry", StringComparison.Ordinal))
                    reported.TrySetResult(JsonDocument.Parse(line[line.IndexOf('{')..]).RootElement);
            },
        });
        await using var client = await McpClient.CreateAsync(transport, cancellationToken: deadline.Token);
        await client.CallToolAsync(AgentToolCatalogue.CheckMachine, cancellationToken: deadline.Token);
        var recorded = await reported.Task.WaitAsync(deadline.Token);
        Assert.Equal("doctor", recorded.GetProperty("command").GetString());
        Assert.Equal("mcp", recorded.GetProperty("interface").GetString());
        Assert.Contains(recorded.GetProperty("outcome").GetString(), new[] { "succeeded", "failed" });
    }

    [Fact]
    public async Task an_event_log_hands_out_only_newer_events_and_waits_for_the_next()
    {
        var log = new EventLog();
        log.Append(new JsonObject { ["event"] = "ready" });
        log.Append(new JsonObject { ["event"] = "reload_pending" });
        var all = await log.ReadAsync(0, TimeSpan.Zero, CancellationToken.None);
        Assert.Equal([1L, 2L], all.Select(entry => entry.Number));
        Assert.Empty(await log.ReadAsync(2, TimeSpan.Zero, CancellationToken.None));
        var waiting = log.ReadAsync(2, TimeSpan.FromSeconds(30), CancellationToken.None);
        log.Append(new JsonObject { ["event"] = "reloaded" });
        var next = await waiting;
        Assert.Equal("reloaded", Assert.Single(next).Event["event"]!.GetValue<string>());
        Assert.Equal(3, next[0].Number);
    }
}
