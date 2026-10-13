using Microsoft.AspNetCore.Mvc;
using Microsoft.AspNetCore.Mvc.RazorPages;

namespace Sample_WebApp.Pages;

/// <summary>
/// What a visitor sees when a request throws outside Development; Program.cs
/// routes failures here. A failed post is re-run against this page, so it
/// accepts one without an antiforgery token.
/// </summary>
[ResponseCache(Duration = 0, Location = ResponseCacheLocation.None, NoStore = true)]
[IgnoreAntiforgeryToken]
public sealed class ErrorModel : PageModel
{
    public string RequestId { get; private set; } = "";

    public void OnGet() => RequestId = HttpContext.TraceIdentifier;

    public void OnPost() => RequestId = HttpContext.TraceIdentifier;
}
