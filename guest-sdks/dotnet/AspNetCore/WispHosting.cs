using Microsoft.AspNetCore.Builder;
using Microsoft.Extensions.Configuration;
using Microsoft.AspNetCore.Hosting;
using Microsoft.AspNetCore.Hosting.Server;
using Microsoft.AspNetCore.Routing;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.DependencyInjection.Extensions;
using Microsoft.Extensions.Logging;
using Microsoft.Extensions.Hosting;

namespace Wisp.AspNetCore;

/// <summary>Hosts ASP.NET applications on WISP's buffered and streaming request transport.</summary>
public static class WispHosting
{
    /// <summary>Creates an ASP.NET builder with host-provided runtime configuration and sandbox logging.</summary>
    /// <param name="applicationName">The assembly name used to discover application endpoints.</param>
    /// <returns>A builder configured to receive requests from WISP without opening a listener.</returns>
    /// <remarks>
    /// The environment name is Production. Configuration files and environment variables are not loaded.
    /// Runtime configuration, including local User Secrets, becomes available after activation;
    /// resolve request-dependent settings through dependency injection rather than capturing them during preparation.
    /// </remarks>
    public static WebApplicationBuilder CreateBuilder(string applicationName)
    {
        var builder = WebApplication.CreateEmptyBuilder(new WebApplicationOptions
        {
            ApplicationName = applicationName,
            EnvironmentName = "Production",
            ContentRootPath = "/",
            Args = []
        });
        ((IConfigurationBuilder)builder.Configuration).Add(new RuntimeConfigurationSource());
        builder.Logging.ClearProviders();
        builder.Logging.AddProvider(new WispLoggerProvider());
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

    /// <summary>Starts the .NET host once, then processes an upload without first collecting its body.</summary>
    public async Task<Response> HandleStreamingAsync(StreamingRequest request)
    {
        await (started ??= StartAsync());
        return await server.SendAsync(request, request.Body, request.CanHaveBody);
    }

    private Task StartAsync()
    {
        if (application.Services.GetServices<IHostedService>().Any(service => service is BackgroundService))
            throw new NotSupportedException("WISP ASP.NET does not support BackgroundService work between completed dispatches. Use hosted services for bounded startup work.");
        return application.StartAsync();
    }

}
