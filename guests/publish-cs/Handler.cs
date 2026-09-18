using System.Threading.Tasks;
using Isoplane;
using Isoplane.Durable;

/// <summary>Returns a literal that the reload test edits.</summary>
public sealed class Handler : IIsoplaneHandler
{
    /// <summary>Returns the current build's marker.</summary>
    public Task<Response> HandleAsync(Request request, Ctx context) =>
        Task.FromResult(Response.Text("first build"));
}
