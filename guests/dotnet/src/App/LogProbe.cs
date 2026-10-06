using System;
using System.Threading.Tasks;
using Microsoft.Extensions.Logging;
using Isoplane;

namespace Isoplane.App;

/// <summary>Emits Console and SDK-provider records on both sides of a host future.</summary>
public static class LogProbe
{
    /// <summary>Completes after the host resumes the logging request.</summary>
    [HttpHandler]
    public static async Task<Response> HandleAsync(Request req, Ctx ctx)
    {
        if (req.Path == "/after-response")
        {
            _ = LogAfterResponse(ctx);
            return Response.Text("returned");
        }
        using var provider = new IsoplaneLoggerProvider();
        var logger = provider.CreateLogger("LoggingFixture");
        Console.WriteLine("console-before");
        logger.LogWarning("sdk-before");
        await Task.Delay(1);
        Console.Error.WriteLine("console-after");
        logger.LogCritical("sdk-after");
        return Response.Text("logged");
    }

    private static async Task LogAfterResponse(Ctx ctx)
    {
        await Task.Delay(1);
        Console.WriteLine("forbidden-after-response");
    }
}
