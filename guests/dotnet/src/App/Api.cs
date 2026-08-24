using System.Threading.Tasks;

namespace Wisp.App;

/// <summary>
/// HTTP fixture returning the requested path. Optional build probes replace
/// that response with their compatibility or activation-randomness observations.
/// </summary>
public sealed class Api
{
    public Task<Response> HandleAsync(Request req, Ctx ctx)
    {
        return Task.FromResult(Response.Text(req.Path));
    }
}
