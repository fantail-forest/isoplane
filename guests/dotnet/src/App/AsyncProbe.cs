using System;
using System.Threading.Tasks;
using Wisp.Durable;

namespace Wisp.App;

/// <summary>Exercises managed continuations on both sides of a host future.</summary>
public sealed class AsyncProbe : IWispHandler
{
    private int _runs;

    /// <summary>Completes only when thread-pool work and an uncaptured await both resume.</summary>
    public async Task<Response> HandleAsync(Request req, Ctx ctx)
    {
        int first = await Task.Run(() => ++_runs).ConfigureAwait(false);
        await ctx.Cache.GetAsync(req.Path).ConfigureAwait(false);
        int second = await Task.Run(() => ++_runs).ConfigureAwait(false);
        if (first != 1 || second != 2)
            throw new InvalidOperationException("Managed work must execute exactly once across the host future.");
        return Response.Text("resumed");
    }
}
