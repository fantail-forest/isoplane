using System;
using System.Text;
using System.Threading.Tasks;
using Isoplane.Durable;

namespace Isoplane.App;

/// <summary>Checks HTTP request fields, including on methods whose responses have no body.</summary>
public sealed class RequestProbe : IIsoplaneHandler
{
    /// <summary>Returns 200 only when the request matches the caller's expected field summary.</summary>
    public async Task<Response> HandleAsync(Request req, Ctx ctx)
    {
        await ctx.Cache.GetAsync(req.Path);
        var summary = new StringBuilder();
        summary.Append(req.Method).Append('\n').Append(req.Url).Append('\n').Append(req.Path).Append('\n');
        foreach (var pair in req.Query)
            summary.Append(pair.Key).Append('=').Append(pair.Value).Append('\n');
        string? expected = null;
        foreach (var pair in req.Headers)
        {
            if (string.Equals(pair.Key, "x-probe", StringComparison.OrdinalIgnoreCase))
                summary.Append(pair.Value).Append('\n');
            if (string.Equals(pair.Key, "x-expected", StringComparison.OrdinalIgnoreCase))
                expected = pair.Value;
        }
        summary.Append(Convert.ToBase64String(req.Body.Span));
        string actual = Convert.ToBase64String(Encoding.UTF8.GetBytes(summary.ToString()));
        return Response.Text(actual, actual == expected ? (ushort)200 : (ushort)422);
    }
}

/// <summary>Exercises explicit streaming selection alongside an ordinary buffered handler.</summary>
public sealed class RequestProbeRouter : IStreamingHttpRouter
{
    private readonly HandlerRouter<RequestProbe> buffered = new();
    /// <summary>Initializes the buffered route.</summary>
    public void Boot() => buffered.Boot();
    /// <summary>Preserves ordinary router dispatch for unselected requests.</summary>
    public Task<Response> Route(Isoplane.Abi.DispatchRecord record, Ctx ctx) => buffered.Route(record, ctx);
    /// <summary>Selects only the fixture's streaming route.</summary>
    public bool StreamsRequest(RequestHead request) => request.Path == "/streaming";
    /// <summary>Reports the streamed bytes through a distinguishable route response.</summary>
    public async Task<Response> RouteStreamingAsync(StreamingRequest request, Ctx ctx)
    {
        using var bytes = new System.IO.MemoryStream();
        await request.Body.CopyToAsync(bytes);
        return Response.Text("stream:" + Convert.ToBase64String(bytes.ToArray()));
    }
}
