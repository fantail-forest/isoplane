using System.ComponentModel;
using Microsoft.Extensions.Options;
using ModelContextProtocol;
using ModelContextProtocol.Server;
using Sample_McpServer.Knowledge;

namespace Sample_McpServer.Tools;

/// <summary>
/// The tools this server offers. Each public method marked
/// <c>[McpServerTool]</c> is one tool: its <c>[Description]</c> texts are what a
/// model reads to decide when and how to call it, its parameters become the
/// tool's input schema, and its return value becomes the result.
/// </summary>
/// <remarks>
/// <para>
/// The class is created for each call, and its constructor parameters come from
/// the services registered in Program.cs. A tool method can take services as
/// parameters too.
/// </para>
/// <para>
/// A tool that waits for something, such as another service or a database,
/// returns a <c>Task</c> and awaits it. The sandbox cannot block, so
/// <c>.Result</c> and <c>.Wait()</c> fail there. Take a
/// <c>CancellationToken</c> and pass it on: it is cancelled when the caller goes
/// away.
/// </para>
/// <code>
/// [McpServerTool(Name = "get_status"), Description("Reports the status of a service.")]
/// public async Task&lt;string&gt; GetStatus(StatusClient client, string service, CancellationToken cancellation) =&gt;
///     await client.GetStatusAsync(service, cancellation);
/// </code>
/// </remarks>
[McpServerToolType]
public sealed class HandbookTools(Handbook handbook, IOptions<HandbookOptions> options)
{
    // ReadOnly says the tool changes nothing, and OpenWorld = false that it
    // reaches nothing outside this server. Both are hints a client may use, for
    // example to decide whether to ask the user before a call. They enforce
    // nothing here.
    [McpServerTool(Name = "search_handbook", Title = "Search the handbook", ReadOnly = true, OpenWorld = false)]
    [Description("Finds handbook articles that mention the given words. Returns the slug, title and an excerpt of each match, best match first. Read a match in full with read_article.")]
    public IReadOnlyList<ArticleSummary> Search(
        [Description("Words to look for, separated by spaces.")] string query,
        [Description("The most matches to return.")] int limit = 3) =>
        // A caller chooses the arguments, so bound them as any request input.
        handbook.Search(query, Math.Clamp(limit, 1, options.Value.MaxResults));

    [McpServerTool(Name = "read_article", Title = "Read a handbook article", ReadOnly = true, OpenWorld = false)]
    [Description("Returns one handbook article in full, as Markdown.")]
    public string Read([Description("The article's slug, as search_handbook returns it.")] string slug) =>
        // The message of an McpException reaches the caller as the tool's error,
        // so word it for the model: say what would work. The message of any other
        // exception is withheld, and the caller learns only that the tool failed.
        handbook.Find(slug)?.Text
        ?? throw new McpException($"The handbook has no article '{slug}'. Find slugs with search_handbook.");
}
