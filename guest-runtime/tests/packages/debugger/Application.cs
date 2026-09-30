using Isoplane.AspNetCore;
using Microsoft.AspNetCore.Builder;

namespace Debugger;

/// <summary>Shared routes for debugger feature scenarios, each run in an isolated host session.</summary>
public static class Application
{
    /// <summary>Creates the debugger fixture without executing its scenarios during warmup.</summary>
    public static WebApplication Create()
    {
        var builder = IsoplaneHosting.CreateBuilder(typeof(Application).Assembly.GetName().Name!);
        var app = builder.Build();
        Features.Application.Map(app);
        Statements.Application.Map(app);
        Watchpoints.Application.Map(app);
        Uncaught.Application.Map(app);
        Protocol.Application.Map(app);
        app.MapGet("/warmup", () => "warm");
        Isoplane.Initialization.Initializing += () => new IsoplaneApplication(app).WarmupAsync("/warmup");
        return app;
    }
}
