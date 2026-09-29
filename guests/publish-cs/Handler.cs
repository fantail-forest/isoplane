using System.Threading.Tasks;
using Isoplane;

/// <summary>Returns a literal that the reload test edits.</summary>
public sealed class Handler : IIsoplaneHandler
{
    /// <summary>Warms response construction before capturing this build's template.</summary>
    public Handler()
    {
        Initialization.Initializing += () =>
        {
            _ = Reply();
            return Task.CompletedTask;
        };
    }

    /// <summary>Returns the current build's marker.</summary>
    public Task<Response> HandleAsync(Request request, Ctx context) =>
        Task.FromResult(Reply());

    private static Response Reply() => Response.Text("first build");
}
