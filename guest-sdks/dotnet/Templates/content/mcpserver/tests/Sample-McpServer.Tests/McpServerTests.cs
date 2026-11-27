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
        // The word comes from an article in the application project, its title
        // or its text, and the articles that should be found from reading the
        // article files here, so the test follows the handbook as you edit it.
        // HandbookTests cover how matches are ranked.
        var articles = ArticleFiles();
        var words = ARareWord(articles);

        // Only the query, as in the README's example, so the tool's own default
        // for "limit" applies. How many results a limit leaves is the next test's
        // subject.
        var found = await SearchAsync(words, limit: null);

        var mentioning = Mentioning(articles, words);
        AssertFoundAmong(mentioning, found);
        if (mentioning.Count > 0) Assert.NotEmpty(found);
    }

    [Fact]
    public async Task A_search_returns_no_more_than_the_configured_maximum()
    {
        // The first word of each article, of its title when its file has none,
        // until there are a few different ones: together they are in more
        // articles than the fixture's "Handbook:MaxResults" allows whenever the
        // handbook has that many, and
        // the caller asks for more still, so exactly the maximum is expected. A
        // smaller handbook cannot show the limit, and the search returns every
        // article that mentions one of the words.
        var articles = ArticleFiles();
        var words = articles.Select(article => SearchableWords(article).FirstOrDefault()).OfType<string>()
            .Distinct().Take(SandboxFixture.MaxResults + 1).ToArray();

        var found = await SearchAsync(words, limit: 50);

        var mentioning = Mentioning(articles, words);
        AssertFoundAmong(mentioning, found);
        Assert.Equal(Math.Min(SandboxFixture.MaxResults, mentioning.Count), found.Length);
    }

    [Fact]
    public async Task Every_article_is_read_in_full()
    {
        // The tool returns an article's file unchanged, line endings and final
        // newline included, so the two are compared exactly.
        foreach (var article in ArticleFiles())
        {
            var result = await sandbox.Mcp.CallToolAsync("read_article", Arguments(("slug", article.Slug)));

            Assert.Equal(article.Text, Text(result));
        }
    }

    [Fact]
    public async Task Reading_an_unknown_article_tells_the_caller_what_to_do()
    {
        // A slug is a file name, and no file name holds a slash, so no article
        // you add can have this one.
        var result = await sandbox.Mcp.CallToolAsync("read_article", Arguments(("slug", "missing/article")));

        Assert.True(result.IsError, sandbox.Diagnostics);
        Assert.Contains("search_handbook", Text(result));
    }

    [Fact]
    public async Task A_browser_is_told_where_the_endpoint_is()
    {
        var page = await sandbox.Client.GetStringAsync("/");

        Assert.Contains("/mcp", page);
    }

    /// <summary>
    /// Each article's slug and the text of its file, read from the application
    /// project's Knowledge/Articles directory without the application's own
    /// classes. The tests take their queries and expected results from these,
    /// not from the wording of the articles the template came with, so they keep
    /// passing as you edit, add and remove articles.
    /// </summary>
    private static (string Slug, string Text)[] ArticleFiles() =>
        Directory.EnumerateFiles(Path.Combine(SandboxFixture.ProjectDirectory, "Knowledge", "Articles"), "*.md")
            .Order(StringComparer.Ordinal)
            .Select(file => (Path.GetFileNameWithoutExtension(file), File.ReadAllText(file)))
            .ToArray();

    /// <summary>
    /// An article's title, as Knowledge/Handbook.cs documents it: the text of its
    /// first line that is not blank when that line is a heading, and its file
    /// name otherwise.
    /// </summary>
    private static string Title((string Slug, string Text) article)
    {
        var firstLine = article.Text.TrimStart().Split('\r', '\n')[0];
        return firstLine.StartsWith("# ", StringComparison.Ordinal) ? firstLine[2..].Trim() : article.Slug;
    }

    /// <summary>
    /// Whether a search for the words should find the article, as
    /// Knowledge/Handbook.cs documents it: one of the words appears in its title
    /// or its text, inside a longer word too, whatever its case.
    /// </summary>
    private static bool Mentions((string Slug, string Text) article, string[] words) =>
        words.Any(word =>
            Title(article).Contains(word, StringComparison.OrdinalIgnoreCase)
            || article.Text.Contains(word, StringComparison.OrdinalIgnoreCase));

    /// <summary>
    /// The words a search can find the article by: those of its text, then
    /// those of its title. An empty file is found by its file name, which is
    /// its title.
    /// </summary>
    private static string[] SearchableWords((string Slug, string Text) article) =>
        $"{article.Text} {Title(article)}".Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries);

    /// <summary>
    /// A word to search for, from the first article that has any: the one the
    /// fewest articles mention, so that the search is likely to return every
    /// match and not only the maximum. Empty only when no article has a word
    /// even in its file name, and a search for nothing finds nothing.
    /// </summary>
    private static string[] ARareWord((string Slug, string Text)[] articles) =>
        articles.Select(SearchableWords).FirstOrDefault(words => words.Length > 0, [])
            .Distinct().Take(50)
            .OrderBy(word => articles.Count(article => Mentions(article, [word])))
            .Take(1).ToArray();

    /// <summary>Calls the search tool, with or without a limit, and returns what it found, best match first.</summary>
    private async Task<ArticleSummary[]> SearchAsync(string[] words, int? limit)
    {
        var arguments = Arguments(("query", string.Join(' ', words)));
        if (limit is not null) arguments["limit"] = limit;

        var result = await sandbox.Mcp.CallToolAsync("search_handbook", arguments);

        Assert.True(result.IsError is not true, sandbox.Diagnostics);
        return JsonSerializer.Deserialize<ArticleSummary[]>(Text(result), JsonSerializerOptions.Web)!;
    }

    /// <summary>
    /// The articles a search for the words should find, by slug, each with its
    /// title, worked out from the article files.
    /// </summary>
    private static Dictionary<string, string> Mentioning((string Slug, string Text)[] articles, string[] words) =>
        articles.Where(article => Mentions(article, words)).ToDictionary(article => article.Slug, Title);

    /// <summary>
    /// Checks that a search found only articles that mention one of the words,
    /// each once and under its own title. It holds for any handbook, so it needs
    /// no change as yours grows.
    /// </summary>
    private static void AssertFoundAmong(Dictionary<string, string> mentioning, ArticleSummary[] found)
    {
        Assert.Equal(found.Select(match => match.Slug).Distinct(), found.Select(match => match.Slug));
        Assert.All(found, match =>
        {
            Assert.True(mentioning.TryGetValue(match.Slug, out var title), $"'{match.Slug}' mentions none of the words searched for.");
            Assert.Equal(title, match.Title);
        });
    }

    private static Dictionary<string, object?> Arguments(params (string Name, object? Value)[] arguments) =>
        arguments.ToDictionary(argument => argument.Name, argument => argument.Value);

    /// <summary>The text of a tool's result. A returned string arrives as it is, and any other value as JSON.</summary>
    private static string Text(CallToolResult result) => Assert.IsType<TextContentBlock>(Assert.Single(result.Content)).Text;
}
