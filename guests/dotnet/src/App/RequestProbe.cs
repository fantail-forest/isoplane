using System;
using System.Text;
using System.Threading.Tasks;
using Isoplane;

namespace Isoplane.App;

/// <summary>Checks HTTP request fields, including on methods whose responses have no body.</summary>
public static class RequestProbe
{
    /// <summary>Returns 200 only when the request matches the caller's expected field summary.</summary>
    [HttpHandler]
    public static async Task<Response> HandleAsync(Request req)
    {
        await Task.Delay(1);
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
