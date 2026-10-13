using Sample_McpServer.Knowledge;

namespace Sample_McpServer.Tests;

/// <summary>
/// Ordinary unit tests. The handbook is application logic that does not depend
/// on Isoplane or MCP, so these run on your machine in milliseconds.
/// </summary>
public sealed class HandbookTests
{
    private static readonly Handbook Handbook = new(
    [
        Article.Parse("tea", "# Making tea\n\nBoil the water first. Tea needs heat."),
        Article.Parse("coffee", "# Making coffee\n\nGrind the beans. Some people prefer tea."),
        Article.Parse("untitled", "No heading here, only water.")
    ]);

    [Fact]
    public void An_article_takes_its_title_from_its_first_heading()
    {
        Assert.Equal("Making tea", Handbook.Find("tea")!.Title);
    }

    [Fact]
    public void An_article_without_a_heading_is_titled_by_its_slug()
    {
        Assert.Equal("untitled", Handbook.Find("untitled")!.Title);
    }

    [Fact]
    public void A_word_in_the_title_outranks_one_in_the_text()
    {
        var matches = Handbook.Search("tea", limit: 10);

        Assert.Equal(["tea", "coffee"], matches.Select(match => match.Slug));
    }

    [Fact]
    public void A_search_matches_any_of_its_words_whatever_their_case()
    {
        var matches = Handbook.Search("BEANS water", limit: 10);

        Assert.Equal(["coffee", "tea", "untitled"], matches.Select(match => match.Slug).Order());
    }

    [Fact]
    public void A_summary_quotes_the_first_paragraph_and_not_the_heading()
    {
        var match = Assert.Single(Handbook.Search("beans", limit: 10));

        Assert.Equal(new ArticleSummary("coffee", "Making coffee", "Grind the beans. Some people prefer tea."), match);
    }

    [Fact]
    public void A_search_stops_at_its_limit()
    {
        Assert.Single(Handbook.Search("water tea", limit: 1));
    }

    [Fact]
    public void An_unknown_slug_finds_nothing()
    {
        Assert.Null(Handbook.Find("cocoa"));
    }
}
