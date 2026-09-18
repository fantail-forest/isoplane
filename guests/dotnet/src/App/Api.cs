using System.Threading.Tasks;

using Isoplane.Durable;

namespace Isoplane.App;

/// <summary>
/// HTTP fixture returning the requested path. Optional build probes replace
/// that response with their compatibility or activation-randomness observations.
/// </summary>
public sealed class Api : IIsoplaneHandler
{
    public Task<Response> HandleAsync(Request req, Ctx ctx)
    {
#if ISOPLANE_ENTROPY_PROBE
        return Task.FromResult(Response.Text(EntropyProbe.Sample()));
#elif ISOPLANE_COMPAT_PROBE
        return Task.FromResult(Response.Json(Compat.Exercise(req.Path), 404));
#else
        return Task.FromResult(Response.Text(req.Path));
#endif
    }
}
