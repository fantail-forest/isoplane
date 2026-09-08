using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Http;
using Wisp.AspNetCore;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Hosting;

namespace AdapterFixture;

public static class Application
{
    public static WebApplication Create()
    {
        var builder = WispHosting.CreateBuilder(typeof(Application).Assembly.GetName().Name!);
        builder.Services.AddScoped<RequestScope>();
        builder.Services.AddHostedService<StartupService>();
        var app = builder.Build();
        app.Use(async (context, next) =>
        {
            if (!StartupService.Started) throw new InvalidOperationException("Hosted startup must complete before request middleware.");
            try { await next(context); }
            catch (OperationCanceledException) when (context.Request.Path == "/cancel")
            {
                context.RequestAborted = CancellationToken.None;
                context.Response.StatusCode = 499;
                await context.Response.WriteAsync("cancelled");
            }
        });
        app.MapGet("/", () => "WISP ASP.NET");
        app.MapPost("/echo", async context =>
        {
            context.Response.OnStarting(() =>
            {
                context.Response.Headers.Append("Set-Cookie", "a=one");
                context.Response.Headers.Append("Set-Cookie", "b=two");
                context.Response.Headers.Append("X-Query", context.Request.Query["tag"]);
                context.Response.Headers.Append("X-Input", context.Request.Headers["X-Input"]);
                return Task.CompletedTask;
            });
            context.Response.StatusCode = 201;
            await context.Request.Body.CopyToAsync(context.Response.Body);
        });
        app.MapGet("/scope", async context =>
        {
            if (RequestScope.Active != 0) throw new InvalidOperationException("A previous request scope survived completion.");
            context.RequestServices.GetRequiredService<RequestScope>();
            await Task.Yield();
            if (RequestScope.Active != 1) throw new InvalidOperationException("A request must own exactly one scope.");
            await context.Response.WriteAsync("isolated");
        });
        app.MapGet("/cancel", async context =>
        {
            context.RequestServices.GetRequiredService<RequestScope>();
            await Task.Delay(Timeout.Infinite, context.RequestAborted);
        });
        return app;
    }
}

internal sealed class RequestScope : IAsyncDisposable
{
    internal static int Active;
    public RequestScope() => Active++;
    public ValueTask DisposeAsync() { Active--; return ValueTask.CompletedTask; }
}

internal sealed class StartupService : IHostedService
{
    internal static bool Started;
    public async Task StartAsync(CancellationToken cancellationToken)
    {
        await Task.Yield();
        Started = true;
    }
    public Task StopAsync(CancellationToken cancellationToken)
    {
        Started = false;
        return Task.CompletedTask;
    }
}
