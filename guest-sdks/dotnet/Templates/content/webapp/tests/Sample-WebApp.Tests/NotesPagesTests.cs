using System.Net;
using AngleSharp;
using AngleSharp.Dom;
using AngleSharp.Html.Dom;
using Microsoft.EntityFrameworkCore;

namespace Sample_WebApp.Tests;

/// <summary>
/// Sandbox tests: a browser using the application while it runs in the real
/// Isoplane sandbox, which is where initialization, the database connection and
/// the Data Protection keys can be observed.
/// </summary>
/// <remarks>
/// The trait lets a quick run leave them out:
/// <c>dotnet test --filter "Category!=Sandbox"</c>. The tests share one database,
/// so each creates the notes it asserts on instead of assuming an empty table.
/// When an assertion needs the application's log to explain a failure, pass
/// <c>sandbox.Diagnostics</c> as its message.
/// </remarks>
[Collection(SandboxCollection.Name)]
[Trait("Category", "Sandbox")]
public sealed class NotesPagesTests(SandboxFixture sandbox)
{
    [Fact]
    public async Task A_note_added_through_the_form_is_listed()
    {
        var browser = sandbox.NewBrowser();
        var home = await browser.OpenAsync(sandbox.Url("/"));

        // Submitting follows the redirect, as a browser does, and returns the page it leads to.
        var listed = await Form(home, "new-note").SubmitAsync(Fields("Buy oat milk", "Two cartons."));

        Assert.True(listed.StatusCode == HttpStatusCode.OK, sandbox.Diagnostics);
        Assert.Equal("Note added.", listed.QuerySelector(".flash")?.TextContent);
        Assert.Contains("Buy oat milk", Titles(listed));
    }

    [Fact]
    public async Task A_note_without_a_title_is_sent_back_with_a_message()
    {
        var browser = sandbox.NewBrowser();
        var home = await browser.OpenAsync(sandbox.Url("/"));

        var refused = await Form(home, "new-note").SubmitAsync(Fields("", "A body with no title above it."));

        Assert.Equal("The Title field is required.", refused.QuerySelector(".note-form .error")?.TextContent);
        // What was typed is still in the form.
        Assert.Equal("A body with no title above it.", refused.QuerySelector<IHtmlTextAreaElement>("textarea")?.Value.Trim());
        await using var database = sandbox.OpenDatabase();
        Assert.False(await database.Notes.AnyAsync(note => note.Body == "A body with no title above it."));
    }

    [Fact]
    public async Task A_note_can_be_edited_and_then_deleted()
    {
        var browser = sandbox.NewBrowser();
        var home = await browser.OpenAsync(sandbox.Url("/"));
        var listed = await Form(home, "new-note").SubmitAsync(Fields("Draft the anouncement", null));

        var edit = await Link(listed, "Draft the anouncement").NavigateAsync();
        var saved = await Form(edit, "edit-note").SubmitAsync(Fields("Draft the announcement", "Spelled properly this time."));

        Assert.Equal("Note saved.", saved.QuerySelector(".flash")?.TextContent);
        Assert.Contains("Draft the announcement", Titles(saved));
        Assert.DoesNotContain("Draft the anouncement", Titles(saved));

        var again = await Link(saved, "Draft the announcement").NavigateAsync();
        var deleted = await Form(again, "delete-note").SubmitAsync();

        Assert.Equal("Note deleted.", deleted.QuerySelector(".flash")?.TextContent);
        Assert.DoesNotContain("Draft the announcement", Titles(deleted));
    }

    [Fact]
    public async Task A_form_posted_without_its_antiforgery_token_is_refused()
    {
        using var forged = new FormUrlEncodedContent(new Dictionary<string, string> { ["Input.Title"] = "Forged" });

        using var response = await sandbox.Client.PostAsync("/", forged);

        Assert.True(response.StatusCode == HttpStatusCode.BadRequest, sandbox.Diagnostics);
    }

    [Fact]
    public async Task The_page_links_a_stylesheet_that_wwwroot_serves()
    {
        var home = await sandbox.NewBrowser().OpenAsync(sandbox.Url("/"));
        var stylesheet = home.QuerySelector<IHtmlLinkElement>("link[rel=stylesheet]")!.Href!;

        using var response = await sandbox.Client.GetAsync(stylesheet);

        Assert.True(response.IsSuccessStatusCode, $"{stylesheet}: {sandbox.Diagnostics}");
        Assert.Equal("text/css", response.Content.Headers.ContentType?.MediaType);
    }

    [Fact]
    public async Task A_note_that_does_not_exist_is_not_found()
    {
        using var response = await sandbox.Client.GetAsync("/Notes/Edit/2147483647");

        Assert.Equal(HttpStatusCode.NotFound, response.StatusCode);
    }

    [Fact]
    public async Task The_data_protection_key_is_kept_in_the_database()
    {
        await using var database = sandbox.OpenDatabase();

        // Initialization created the key and stored it, so the next build of the
        // application reads it and accepts what this one protected.
        Assert.Equal(1, await database.DataProtectionKeys.CountAsync());
    }

    private static IHtmlFormElement Form(IDocument page, string id) =>
        page.QuerySelector<IHtmlFormElement>($"form#{id}") ?? throw new InvalidOperationException($"No form '{id}' in:\n{page.ToHtml()}");

    private static IHtmlAnchorElement Link(IDocument page, string text) =>
        page.QuerySelectorAll<IHtmlAnchorElement>(".notes a").First(link => link.TextContent == text);

    private static IEnumerable<string> Titles(IDocument page) =>
        page.QuerySelectorAll(".notes a").Select(link => link.TextContent);

    /// <summary>Form fields by name. A [BindProperty] named Input binds fields named "Input.…".</summary>
    private static Dictionary<string, string> Fields(string title, string? body) =>
        new() { ["Input.Title"] = title, ["Input.Body"] = body ?? "" };
}
