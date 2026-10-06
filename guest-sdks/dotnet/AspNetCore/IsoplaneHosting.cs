using System.Reflection;
using System.Runtime.CompilerServices;
using Microsoft.AspNetCore.Builder;
using Microsoft.Extensions.Configuration;
using Microsoft.AspNetCore.Hosting;
using Microsoft.AspNetCore.Hosting.Server;
using Microsoft.AspNetCore.Routing;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.DependencyInjection.Extensions;
using Microsoft.Extensions.Logging;
using Microsoft.Extensions.Hosting;

namespace Isoplane.AspNetCore;

/// <summary>Hosts ASP.NET applications on Isoplane's buffered and streaming request transport.</summary>
public static class IsoplaneHosting
{
    /// <summary>Creates the builder for an application's <c>Program.cs</c>.</summary>
    /// <param name="args">The entry point's arguments. Isoplane starts an application with none.</param>
    /// <returns>A builder configured to receive requests from Isoplane without opening a listener.</returns>
    /// <remarks>
    /// Configuration is layered as in ASP.NET: <c>appsettings.json</c>, then
    /// <c>appsettings.{Environment}.json</c>, then the settings the platform supplies. In local
    /// development those are the project's User Secrets and its launch profile's environment
    /// variables. The environment name is the supplied <c>ASPNETCORE_ENVIRONMENT</c> or
    /// <c>DOTNET_ENVIRONMENT</c> setting, and Production without one. The <c>Logging</c>
    /// configuration section sets log levels. Settings read while the application starts are
    /// captured in its template; platform settings are read again when an instance activates.
    /// End the entry point with <c>await app.RunAsync()</c>.
    /// <para>
    /// .NET tools that run the entry point on the developer's machine, such as <c>dotnet ef</c>
    /// or an in-process test host, get a builder that reads <c>appsettings</c> files, User
    /// Secrets in Development and environment variables as ASP.NET does, and logs to the console.
    /// </para>
    /// </remarks>
    [MethodImpl(MethodImplOptions.NoInlining)]
    public static WebApplicationBuilder CreateBuilder(string[] args)
    {
        if (!OperatingSystem.IsWasi()) return CreateToolingBuilder(args, Assembly.GetCallingAssembly());
        var settings = RuntimeConfiguration.ReadValues();
        var environment = settings.GetValueOrDefault("ASPNETCORE_ENVIRONMENT")
            ?? settings.GetValueOrDefault("DOTNET_ENVIRONMENT") ?? Environments.Production;
        var name = ApplicationStartup.Current?.ApplicationName ?? Assembly.GetCallingAssembly().GetName().Name!;
        return CreateBuilder(name, new RuntimeConfigurationSource(), environment, args, files: true);
    }

    // Outside the sandbox no platform supplies settings or carries logs, so the
    // application reads what an ordinary ASP.NET application would.
    private static WebApplicationBuilder CreateToolingBuilder(string[] args, Assembly application)
    {
        var environment = Environment.GetEnvironmentVariable("ASPNETCORE_ENVIRONMENT")
            ?? Environment.GetEnvironmentVariable("DOTNET_ENVIRONMENT") ?? Environments.Production;
        var builder = WebApplication.CreateEmptyBuilder(new WebApplicationOptions
        {
            ApplicationName = application.GetName().Name,
            EnvironmentName = environment,
            Args = args
        });
        builder.Configuration.SetBasePath(builder.Environment.ContentRootPath);
        builder.Configuration.AddJsonFile("appsettings.json", optional: true, reloadOnChange: false);
        builder.Configuration.AddJsonFile($"appsettings.{environment}.json", optional: true, reloadOnChange: false);
        if (builder.Environment.IsDevelopment()) builder.Configuration.AddUserSecrets(application, optional: true);
        builder.Configuration.AddEnvironmentVariables();
        builder.Logging.AddConfiguration(builder.Configuration.GetSection("Logging"));
        builder.Logging.AddConsole();
        builder.Services.AddRouting();
        builder.WebHost.UseIsoplane();
        return builder;
    }

    /// <summary>Creates an ASP.NET builder with host-provided runtime configuration and sandbox logging.</summary>
    /// <param name="applicationName">The assembly name used to discover application endpoints.</param>
    /// <returns>A builder configured to receive requests from Isoplane without opening a listener.</returns>
    /// <remarks>
    /// The environment name is Production. Configuration files and environment variables are not loaded.
    /// Configuration, including local User Secrets, is available during template initialization
    /// and activation. Captured settings belong to the deployment that prepared the template.
    /// </remarks>
    public static WebApplicationBuilder CreateBuilder(string applicationName) =>
        CreateBuilder(applicationName, new RuntimeConfigurationSource());

    internal static WebApplicationBuilder CreateBuilder(string applicationName, IConfigurationSource configuration) =>
        CreateBuilder(applicationName, configuration, Environments.Production, [], files: false);

    private static WebApplicationBuilder CreateBuilder(string applicationName, IConfigurationSource configuration, string environment, string[] args, bool files)
    {
        var builder = WebApplication.CreateEmptyBuilder(new WebApplicationOptions
        {
            ApplicationName = applicationName,
            EnvironmentName = environment,
            ContentRootPath = "/",
            Args = args
        });
        if (files)
        {
            // The package's content files are served at the content root; nothing replaces them while an instance runs.
            builder.Configuration.SetBasePath(builder.Environment.ContentRootPath);
            builder.Configuration.AddJsonFile("appsettings.json", optional: true, reloadOnChange: false);
            builder.Configuration.AddJsonFile($"appsettings.{environment}.json", optional: true, reloadOnChange: false);
        }
        ((IConfigurationBuilder)builder.Configuration).Add(configuration);
        builder.Logging.ClearProviders();
        if (files) builder.Logging.AddConfiguration(builder.Configuration.GetSection("Logging"));
        builder.Logging.AddProvider(new IsoplaneLoggerProvider());
        builder.Services.AddRouting();
        builder.WebHost.UseIsoplane();
        return builder;
    }

    /// <summary>Runs a GET through the started application's pipeline and discards its response body.</summary>
    /// <param name="application">The application whose entry point is awaiting <c>RunAsync</c>.</param>
    /// <param name="path">The application-relative path and optional query, beginning with a slash.</param>
    /// <remarks>
    /// Call it from an <see cref="Initialization.Initializing"/> callback, so the work a first
    /// request does is captured in the template every instance starts from. Middleware, handlers,
    /// serialization and completion callbacks run normally.
    /// </remarks>
    /// <exception cref="ArgumentException">The path is not application-relative.</exception>
    /// <exception cref="InvalidOperationException">The application has not started.</exception>
    /// <exception cref="HttpRequestException">The endpoint returns a status outside 200–299.</exception>
    public static Task WarmupAsync(this WebApplication application, string path)
    {
        var server = application.Services.GetRequiredService<IServer>() as IsoplaneServer
            ?? throw new InvalidOperationException("Create the application with IsoplaneHosting.CreateBuilder.");
        if (!server.Started)
            throw new InvalidOperationException("WarmupAsync needs the started application. Call it from an Isoplane.Initialization.Initializing callback, which runs once RunAsync has started the host.");
        return new IsoplaneApplication(application).WarmupAsync(path);
    }

    /// <summary>Replaces the server with Isoplane's host-delivered request transport.</summary>
    public static IWebHostBuilder UseIsoplane(this IWebHostBuilder builder)
    {
        var startup = ApplicationStartup.Current;
        return builder.ConfigureServices(services =>
        {
            if (startup is not null) services.AddSingleton(startup);
            services.TryAddSingleton<Func<ResponseTransport>>(_ => () => new HostResponseTransport());
            services.RemoveAll<IServer>();
            services.AddSingleton<IServer, IsoplaneServer>();
            services.RemoveAll<IHostLifetime>();
            services.AddSingleton<IHostLifetime, IsoplaneLifetime>();
        });
    }
}

/// <summary>Runs a built application with one host startup shared by preparation, warmup, and requests.</summary>
public sealed class IsoplaneApplication
{
    private readonly WebApplication application;
    private readonly IsoplaneServer server;

    /// <summary>Retains stable application state without starting hosted services.</summary>
    public IsoplaneApplication(WebApplication application)
    {
        this.application = application;
        server = application.Services.GetRequiredService<IServer>() as IsoplaneServer
            ?? throw new InvalidOperationException("Register the Isoplane server with UseIsoplane before building the application.");
        foreach (var source in ((IEndpointRouteBuilder)application).DataSources)
            _ = source.Endpoints;
    }

    internal Task StartAsync() => server.StartApplicationAsync(application);

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
        var request = Request.FromDispatch(new Isoplane.Abi.DispatchRecord
        {
            Kind = Isoplane.Abi.DispatchKind.Http,
            Method = "GET",
            Path = query < 0 ? path : path[..query],
            Url = path
        });
        await server.SendAsync(request, new WarmupResponseTransport());
    }
}
