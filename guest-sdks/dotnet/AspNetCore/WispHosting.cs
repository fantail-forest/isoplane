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
    /// Configuration, including local User Secrets, is available during template initialization
    /// and activation. Captured settings belong to the deployment that prepared the template.
    /// </remarks>
    public static WebApplicationBuilder CreateBuilder(string applicationName) =>
        CreateBuilder(applicationName, new RuntimeConfigurationSource());

    internal static WebApplicationBuilder CreateBuilder(string applicationName, IConfigurationSource configuration)
    {
        var builder = WebApplication.CreateEmptyBuilder(new WebApplicationOptions
        {
            ApplicationName = applicationName,
            EnvironmentName = "Production",
            ContentRootPath = "/",
            Args = []
        });
        ((IConfigurationBuilder)builder.Configuration).Add(configuration);
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

/// <summary>Runs a built application, starting hosted services on its first warmup or host request.</summary>
public sealed class WispApplication
{
    private readonly WebApplication application;
    private readonly WispServer server;

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
        await server.StartApplicationAsync(application);
        return await server.SendAsync(request);
    }

    /// <summary>Starts the .NET host once, then processes an upload without first collecting its body.</summary>
    public async Task<Response> HandleStreamingAsync(StreamingRequest request)
    {
        await server.StartApplicationAsync(application);
        return await server.SendAsync(request, request.Body, request.CanHaveBody);
    }

    /// <summary>Runs a GET through the application pipeline and discards its response body.</summary>
    /// <param name="path">The application-relative path and optional query, beginning with a slash.</param>
    /// <remarks>Use from an initialization hook for read endpoints. Hosted services start once and
    /// remain started for subsequent requests. Middleware, handlers, serialization and completion
    /// callbacks run normally; application I/O still follows the preparation policy.</remarks>
    /// <exception cref="ArgumentException">The path is not application-relative.</exception>
    /// <exception cref="HttpRequestException">The endpoint returns a status outside 200–299.</exception>
    public async Task WarmupAsync(string path)
    {
        if (string.IsNullOrEmpty(path) || !path.StartsWith('/') || path.StartsWith("//", StringComparison.Ordinal))
            throw new ArgumentException("Use an application-relative path beginning with a slash.", nameof(path));
        await server.StartApplicationAsync(application);
        var query = path.IndexOf('?');
        var request = Request.FromDispatch(new Wisp.Abi.DispatchRecord
        {
            Kind = Wisp.Abi.DispatchKind.Http,
            Method = "GET",
            Path = query < 0 ? path : path[..query],
            Url = path
        });
        await server.SendAsync(request, new WarmupResponseTransport());
    }
}
