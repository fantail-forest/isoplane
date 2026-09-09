using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Hosting;
using Microsoft.AspNetCore.Hosting.Server;
using Microsoft.AspNetCore.Routing;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.DependencyInjection.Extensions;
using Microsoft.Extensions.Logging;
using Microsoft.Extensions.Hosting;

namespace Wisp.AspNetCore;

/// <summary>Registers ASP.NET hosting on WISP's buffered request transport.</summary>
public static class WispHosting
{
    /// <summary>Creates a builder without configuration files, environment variables or a listener.</summary>
    public static WebApplicationBuilder CreateBuilder(string applicationName)
    {
        var builder = WebApplication.CreateEmptyBuilder(new WebApplicationOptions
        {
            ApplicationName = applicationName,
            EnvironmentName = "Production",
            ContentRootPath = "/",
            Args = []
        });
        builder.Logging.ClearProviders();
        builder.Services.AddRouting();
        builder.WebHost.UseWisp();
        return builder;
    }

    /// <summary>Replaces the server with WISP's host-delivered request transport.</summary>
    public static IWebHostBuilder UseWisp(this IWebHostBuilder builder) =>
        builder.ConfigureServices(services =>
        {
            services.TryAddSingleton<Func<ResponseTransport>>(_ => () => new HostResponseTransport());
            services.RemoveAll<IServer>();
            services.AddSingleton<IServer, WispServer>();
        });
}

/// <summary>Runs a built application; hosted services start only after activation.</summary>
public sealed class WispApplication
{
    private readonly WebApplication application;
    private readonly WispServer server;
    private Task? started;

    /// <summary>Retains stable application state without starting hosted services.</summary>
    public WispApplication(WebApplication application)
    {
        this.application = application;
        server = application.Services.GetRequiredService<IServer>() as WispServer
            ?? throw new InvalidOperationException("Register the WISP server with UseWisp before building the application.");
        foreach (var source in ((IEndpointRouteBuilder)application).DataSources)
            _ = source.Endpoints;
    }

    /// <summary>Starts the .NET host once, then processes a buffered host request.</summary>
    public async Task<Response> HandleAsync(Request request)
    {
        await (started ??= StartAsync());
        return await server.SendAsync(request);
    }

    private Task StartAsync()
    {
        if (application.Services.GetServices<IHostedService>().Any(service => service is BackgroundService))
            throw new NotSupportedException("WISP ASP.NET does not support BackgroundService work between completed dispatches. Use hosted services for bounded startup work.");
        return application.StartAsync();
    }

}
