using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using Isoplane;

namespace Isoplane.App;

/// <summary>
/// Answers a request after its client has gone, and reports what the handler saw on the
/// instance's next request.
/// </summary>
public static class DepartureProbe
{
    private static string observed = "";

    /// <summary>Waits for the client to leave, then returns or streams a response.</summary>
    public static async Task<Response> HandleAsync(string path, Request request)
    {
        var departed = new TaskCompletionSource();
        using (request.Aborted.Register(() => departed.SetResult())) await departed.Task;
        observed = "aborted";
        if (path == "/stream")
        {
            var stream = Response.Stream(200, Array.Empty<KeyValuePair<string, string>>());
            await stream.WriteAsync("late"u8.ToArray());
            observed += ";written";
            try { await stream.WriteAsync("late"u8.ToArray(), request.Aborted); }
            catch (OperationCanceledException error) { observed += ";" + error.GetType().Name; }
            return stream.Complete();
        }
        observed += ";returned";
        return Response.Text("late");
    }

    /// <summary>What the last departed request saw.</summary>
    public static Task<Response> ReportAsync() => Task.FromResult(Response.Text(observed));
}
