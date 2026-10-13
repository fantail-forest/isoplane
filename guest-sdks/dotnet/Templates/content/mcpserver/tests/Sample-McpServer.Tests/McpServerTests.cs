using System.Text.Json;
using ModelContextProtocol.Protocol;
using Sample_McpServer.Knowledge;

namespace Sample_McpServer.Tests;

/// <summary>
/// Sandbox tests: an MCP client calling the server while it runs in the real
/// Isoplane sandbox, which is where initialization and the stateless transport
/// can be observed.
/// </summary>
/// <remarks>
/// The trait lets a quick run leave them out:
/// <c>dotnet test --filter "Category!=Sandbox"</c>. When an assertion needs the
/// application's log to explain a failure, pass <c>sandbox.Diagnostics</c> as its message.
/// </remarks>
[Collection(SandboxCollection.Name)]
[Trait("Category", "Sandbox")]
public sealed class McpServerTests(SandboxFixture sandbox)
{
    [Fact]
    public async Task The_server_offers_its_tools_with_their_descriptions()
    {
        var tools = await sandbox.Mcp.ListToolsAsync();

        Assert.Equal(["read_article", "search_handbook"], tools.Select(tool => tool.Name).Order());
        Assert.All(tools, tool => Assert.False(string.IsNullOrWhiteSpace(tool.Description), sandbox.Diagnostics));
    }

    [Fact]
    public async Task A_search_finds_the_articles_loaded_during_initialization()
    {
        var result = await sandbox.Mcp.CallToolAsync("search_handbook", Arguments(("query", "release")));

        Assert.True(result.IsError is not true, sandbox.Diagnostics);
        var matches = JsonSerializer.Deserialize<ArticleSummary[]>(Text(result), JsonSerializerOptions.Web)!;
        Assert.Equal("release-checklist", matches[0].Slug);
        Assert.Equal("Release checklist", matches[0].Title);
    }

    [Fact]
    public async Task A_search_returns_no_more_than_the_configured_maximum()
    {
        // Every article mentions "the", and the caller asks for more than the
        // fixture's "Handbook:MaxResults" allows.
        var result = await sandbox.Mcp.CallToolAsync("search_handbook", Arguments(("query", "the"), ("limit", 50)));

        var matches = JsonSerializer.Deserialize<ArticleSummary[]>(Text(result), JsonSerializerOptions.Web)!;
        Assert.Equal(SandboxFixture.MaxResults, matches.Length);
    }

    [Fact]
    public async Task An_article_is_read_in_full()
    {
        var result = await sandbox.Mcp.CallToolAsync("read_article", Arguments(("slug", "code-review")));

        Assert.StartsWith("# Code review", Text(result));
    }

    [Fact]
    public async Task Reading_an_unknown_article_tells_the_caller_what_to_do()
    {
        var result = await sandbox.Mcp.CallToolAsync("read_article", Arguments(("slug", "holiday-policy")));

        Assert.True(result.IsError, sandbox.Diagnostics);
        Assert.Contains("search_handbook", Text(result));
    }

    [Fact]
    public async Task A_browser_is_told_where_the_endpoint_is()
    {
        var page = await sandbox.Client.GetStringAsync("/");

        Assert.Contains("/mcp", page);
    }

    private static Dictionary<string, object?> Arguments(params (string Name, object? Value)[] arguments) =>
        arguments.ToDictionary(argument => argument.Name, argument => argument.Value);

    /// <summary>The text of a tool's result. A returned string arrives as it is, and any other value as JSON.</summary>
    private static string Text(CallToolResult result) => Assert.IsType<TextContentBlock>(Assert.Single(result.Content)).Text;
}
