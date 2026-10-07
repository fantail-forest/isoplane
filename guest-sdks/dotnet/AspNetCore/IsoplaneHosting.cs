using System.Reflection;
using System.Runtime.CompilerServices;
using Microsoft.AspNetCore.Builder;
using Microsoft.Extensions.Configuration;
using Microsoft.AspNetCore.Hosting;
using Microsoft.AspNetCore.Hosting.Server;
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
        return CreateBuilder(name, new RuntimeConfigurationSource(), environment, args);
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

    /// <summary>The sandbox builder over another source of platform settings, which tests substitute.</summary>
    internal static WebApplicationBuilder CreateBuilder(string applicationName, IConfigurationSource configuration) =>
        CreateBuilder(applicationName, configuration, Environments.Production, []);

    private static WebApplicationBuilder CreateBuilder(string applicationName, IConfigurationSource configuration, string environment, string[] args)
    {
        var builder = WebApplication.CreateEmptyBuilder(new WebApplicationOptions
        {
            ApplicationName = applicationName,
            EnvironmentName = environment,
            ContentRootPath = "/",
            Args = args
        });
        // The package's content files are served at the content root; nothing replaces them while an instance runs.
        builder.Configuration.SetBasePath(builder.Environment.ContentRootPath);
        builder.Configuration.AddJsonFile("appsettings.json", optional: true, reloadOnChange: false);
        builder.Configuration.AddJsonFile($"appsettings.{environment}.json", optional: true, reloadOnChange: false);
        ((IConfigurationBuilder)builder.Configuration).Add(configuration);
        builder.Logging.ClearProviders();
        builder.Logging.AddConfiguration(builder.Configuration.GetSection("Logging"));
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
    /// serialization and completion callbacks run normally. The request's host is <c>localhost</c>.
    /// </remarks>
    /// <exception cref="ArgumentException">The path is not application-relative.</exception>
    /// <exception cref="InvalidOperationException">The application has not started.</exception>
    /// <exception cref="HttpRequestException">The endpoint returns a status outside 200–299.</exception>
    public static async Task WarmupAsync(this WebApplication application, string path)
    {
        var server = application.Services.GetRequiredService<IServer>() as IsoplaneServer
            ?? throw new InvalidOperationException("Create the application with IsoplaneHosting.CreateBuilder.");
        if (!server.Started)
            throw new InvalidOperationException("WarmupAsync needs the started application. Call it from an Isoplane.Initialization.Initializing callback, which runs once RunAsync has started the host.");
        if (string.IsNullOrEmpty(path) || !path.StartsWith('/') || path.StartsWith("//", StringComparison.Ordinal))
            throw new ArgumentException("Use an application-relative path beginning with a slash.", nameof(path));
        var query = path.IndexOf('?');
        var request = Request.FromDispatch(new Isoplane.Abi.DispatchRecord
        {
            Kind = Isoplane.Abi.DispatchKind.Http,
            Method = "GET",
            Path = query < 0 ? path : path[..query],
            Url = path,
            // Every real request names a host, and code that builds absolute URLs,
            // such as Blazor's navigation, fails without one.
            Headers = [new("Host", "localhost")]
        });
        await server.SendAsync(request, new WarmupResponseTransport());
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
            services.AddHostedService<BackgroundWorkGuard>();
        });
    }
}
