namespace Sample_McpServer.Knowledge;

/// <summary>One handbook article, read from a Markdown file.</summary>
/// <param name="Slug">The file's name without its extension. It identifies the article.</param>
/// <param name="Title">The text of the article's first heading.</param>
/// <param name="Text">The whole article, as Markdown.</param>
public sealed record Article(string Slug, string Title, string Text)
{
    private const int ExcerptLength = 160;

    /// <summary>Reads an article's title from its first line when that line is a heading; the slug stands in otherwise.</summary>
    public static Article Parse(string slug, string text)
    {
        var firstLine = text.AsSpan().TrimStart();
        var end = firstLine.IndexOfAny('\r', '\n');
        if (end >= 0) firstLine = firstLine[..end];
        var title = firstLine.StartsWith("# ") ? firstLine[2..].Trim().ToString() : slug;
        return new Article(slug, title, text);
    }

    /// <summary>The article's identity and the beginning of its first paragraph.</summary>
    public ArticleSummary Summarize()
    {
        var paragraph = Text.Split('\n', StringSplitOptions.TrimEntries | StringSplitOptions.RemoveEmptyEntries)
            .FirstOrDefault(line => !line.StartsWith('#')) ?? "";
        return new ArticleSummary(Slug, Title, paragraph.Length <= ExcerptLength ? paragraph : paragraph[..ExcerptLength] + "…");
    }
}

/// <summary>What a search returns for a match: enough to decide whether to read the article.</summary>
public sealed record ArticleSummary(string Slug, string Title, string Excerpt);

/// <summary>Settings from the "Handbook" configuration section.</summary>
public sealed class HandbookOptions
{
    /// <summary>The most articles one search returns.</summary>
    public int MaxResults { get; set; } = 5;
}

/// <summary>
/// The articles the server answers from. It is ordinary application logic and
/// knows nothing of MCP or Isoplane, so unit tests exercise it directly.
/// </summary>
public sealed class Handbook(IEnumerable<Article> articles)
{
    private readonly Dictionary<string, Article> articles =
        articles.ToDictionary(article => article.Slug, StringComparer.OrdinalIgnoreCase);

    public int Count => articles.Count;

    /// <summary>Reads every Markdown file in a directory as an article.</summary>
    public static async Task<Handbook> LoadAsync(string directory, CancellationToken cancellation = default)
    {
        var loaded = new List<Article>();
        foreach (var file in Directory.EnumerateFiles(directory, "*.md"))
            loaded.Add(Article.Parse(Path.GetFileNameWithoutExtension(file), await File.ReadAllTextAsync(file, cancellation)));
        return new Handbook(loaded);
    }

    /// <summary>The article with this slug, or null when there is none.</summary>
    public Article? Find(string slug) => articles.GetValueOrDefault(slug);

    /// <summary>
    /// The articles that mention any of the query's words, best match first. A
    /// word in the title counts for three in the text.
    /// </summary>
    public IReadOnlyList<ArticleSummary> Search(string query, int limit)
    {
        var words = query.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries);
        return articles.Values
            .Select(article => (Article: article, Score: words.Sum(word => 3 * Occurrences(article.Title, word) + Occurrences(article.Text, word))))
            .Where(match => match.Score > 0)
            .OrderByDescending(match => match.Score)
            .ThenBy(match => match.Article.Slug, StringComparer.Ordinal)
            .Take(limit)
            .Select(match => match.Article.Summarize())
            .ToArray();
    }

    private static int Occurrences(string text, string word)
    {
        var count = 0;
        for (var at = text.IndexOf(word, StringComparison.OrdinalIgnoreCase); at >= 0;
             at = text.IndexOf(word, at + word.Length, StringComparison.OrdinalIgnoreCase))
            count++;
        return count;
    }
}
