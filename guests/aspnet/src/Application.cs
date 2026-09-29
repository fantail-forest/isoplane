using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Http;
using Isoplane.AspNetCore;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Hosting;
using Microsoft.Extensions.Logging;

namespace AdapterFixture;

/// <summary>Exercises the ASP.NET transport, request scopes, and hosted startup.</summary>
public static class Application
{
    /// <summary>Builds the transport fixture and warms its ordinary and scoped read routes.</summary>
    public static WebApplication Create()
    {
        var builder = IsoplaneHosting.CreateBuilder(typeof(Application).Assembly.GetName().Name!);
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
        app.MapGet("/", () => "Isoplane ASP.NET");
        app.MapGet("/logs", async context =>
        {
            context.RequestServices.GetRequiredService<ILoggerFactory>()
                .CreateLogger("AspNetLoggingFixture").LogWarning("aspnet-application-log");
            await context.Response.WriteAsync("aspnet logged");
        });
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
        app.MapGet("/stream", async context =>
        {
            await context.Response.Body.WriteAsync("A"u8.ToArray());
            await context.Response.Body.FlushAsync();
            using var gate = new System.Net.Sockets.TcpClient();
            await gate.ConnectAsync(System.Net.IPAddress.Loopback, int.Parse(context.Request.Query["port"].ToString()), context.RequestAborted);
            byte[] release = new byte[1];
            await gate.GetStream().ReadExactlyAsync(release, context.RequestAborted);
            if (release[0] == 0) throw new InvalidOperationException("The streaming fixture failed after publishing its first chunk.");
            await context.Response.Body.WriteAsync("B"u8.ToArray());
        });
        Isoplane.Initialization.Initializing += async () =>
        {
            var application = new IsoplaneApplication(app);
            await application.WarmupAsync("/");
            await application.WarmupAsync("/scope");
        };
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
