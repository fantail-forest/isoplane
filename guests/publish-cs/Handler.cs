using System.Threading.Tasks;
using Wisp;
using Wisp.Durable;

/// <summary>Returns a literal that the reload test edits.</summary>
public sealed class Handler : IWispHandler
{
    /// <summary>Returns the current build's marker.</summary>
    public Task<Response> HandleAsync(Request request, Ctx context) =>
        Task.FromResult(Response.Text("first build"));
}
