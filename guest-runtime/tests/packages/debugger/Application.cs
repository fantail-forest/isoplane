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
        StatementAwaits.Application.Map(app);
        StatementHandlers.Application.Map(app);
        StatementIterators.Application.Map(app);
        app.MapGet("/hotreload-callers", HotReload.Callers.Handle);
        app.MapGet("/hotreload-constructor-callers", HotReload.Callers.Construct);
        app.MapGet("/hotreload-iterators", HotReload.Iterators.Handle);
        app.MapGet("/hotreload", HotReload.Application.Handle);
        app.MapGet("/hotreload-layout", Debugger.HotReload.Application.HandleLayout);
        app.MapGet("/hotreload-try", HotReload.Application.HandleTry);
        app.MapGet("/hotreload-async", HotReload.Application.HandleAsync);
        app.MapGet("/hotreload-unchanged-handler", HotReload.Application.HandleUnchanged);
        Watchpoints.Application.Map(app);
        Uncaught.Application.Map(app);
        Protocol.Application.Map(app);
        app.MapGet("/warmup", () => "warm");
        Isoplane.Initialization.Initializing += () => app.WarmupAsync("/warmup");
        await app.RunAsync();
    }
}
