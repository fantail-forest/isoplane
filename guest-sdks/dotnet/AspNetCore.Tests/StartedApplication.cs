using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Hosting.Server;
using Microsoft.Extensions.DependencyInjection;

namespace Isoplane.AspNetCore.Tests;

/// <summary>Sends requests to a built application as the runtime does once its entry point awaits <c>RunAsync</c>.</summary>
internal static class StartedApplication
{
    /// <summary>Starts the host on first use, which is the first thing <c>RunAsync</c> does, and returns its server.</summary>
    internal static async Task<IsoplaneServer> StartedAsync(this WebApplication application)
    {
        var server = (IsoplaneServer)application.Services.GetRequiredService<IServer>();
        if (!server.Started) await application.StartAsync();
        return server;
    }

    /// <summary>Sends <paramref name="request"/> through the started application's server.</summary>
    internal static async Task<Response> HandleAsync(this WebApplication application, Request request) =>
        await (await application.StartedAsync()).SendAsync(request);
}
