using System;
using System.Threading.Tasks;
using Microsoft.Extensions.Logging;
using Isoplane.Durable;

namespace Isoplane.App;

/// <summary>Emits Console and SDK-provider records on both sides of a host future.</summary>
public sealed class LogProbe : IIsoplaneHandler
{
    /// <summary>Completes after the host resumes the logging request.</summary>
    public async Task<Response> HandleAsync(Request req, Ctx ctx)
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
        await ctx.Cache.GetAsync(req.Path);
        Console.Error.WriteLine("console-after");
        logger.LogCritical("sdk-after");
        return Response.Text("logged");
    }

    private static async Task LogAfterResponse(Ctx ctx)
    {
        await ctx.Cache.GetAsync("late-log");
        Console.WriteLine("forbidden-after-response");
    }
}
