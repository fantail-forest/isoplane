using System.Threading.Tasks;
using Isoplane.AspNetCore;
using Microsoft.AspNetCore.Builder;

namespace Debugger;

/// <summary>Shared routes for debugger feature scenarios, each run in an isolated host session.</summary>
public static class Application
{
    /// <summary>Runs the debugger fixture without executing its scenarios during warmup.</summary>
    public static async Task Main(string[] args)
    {
        var builder = IsoplaneHosting.CreateBuilder(args);
        var app = builder.Build();
        ValueTasks.Application.Map(app);
        Pooled.Application.Map(app);
        PooledComposed.Application.Map(app);
        Async.Application.Map(app);
        Composed.Application.Map(app);
        Features.Application.Map(app);
        Statements.Application.Map(app);
        Watchpoints.Application.Map(app);
        Uncaught.Application.Map(app);
        Protocol.Application.Map(app);
        app.MapGet("/warmup", () => "warm");
        Isoplane.Initialization.Initializing += () => app.WarmupAsync("/warmup");
        await app.RunAsync();
    }
}
