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

/// <summary>
/// Starts an ASP.NET application on Isoplane. Call <see cref="CreateBuilder(string[])"/> in <c>Program.cs</c>
/// where an ordinary application calls <c>WebApplication.CreateBuilder</c>.
/// </summary>
/// <remarks>
/// <para>
/// The builder it returns is an ordinary <c>WebApplicationBuilder</c>, so services, configuration,
/// logging, middleware and endpoints are set up as usual. What differs is that Isoplane delivers
/// requests to the application itself: the application opens no port and runs no web server of its
/// own.
/// </para>
/// <para>
/// <c>dotnet new isoplane-webapi</c> creates a complete project, and
/// <see href="https://isoplane.dev/docs/onboard-an-application">Bring an existing ASP.NET application</see>
/// covers moving one over.
/// </para>
/// </remarks>
/// <example>
/// <code>
/// using Isoplane.AspNetCore;
///
/// var builder = IsoplaneHosting.CreateBuilder(args);
/// var app = builder.Build();
///
/// app.MapGet("/", () => "Hello from Isoplane!");
///
/// await app.RunAsync();
/// </code>
/// </example>
public static class IsoplaneHosting
{
    /// <summary>
    /// Creates the application's builder. Use it in <c>Program.cs</c> in place of
    /// <c>WebApplication.CreateBuilder</c>.
    /// </summary>
    /// <param name="args">The arguments of <c>Program.cs</c>; pass <c>args</c>. Isoplane starts an application with none.</param>
    /// <returns>
    /// An ordinary <c>WebApplicationBuilder</c>, set up so that Isoplane delivers the application's
    /// requests and collects its logs.
    /// </returns>
    /// <remarks>
    /// <para>
    /// Finish <c>Program.cs</c> with <c>await app.RunAsync()</c>. The synchronous <c>app.Run()</c>
    /// would block, which an application on Isoplane cannot do, and the build rejects it.
    /// </para>
    /// <para>
    /// <c>builder.Configuration</c> is layered as in any ASP.NET application, a later source
    /// overriding an earlier one: <c>appsettings.json</c>, then
    /// <c>appsettings.{Environment}.json</c>, then the settings Isoplane supplies. In local
    /// development the supplied settings are the project's User Secrets, the
    /// <c>environmentVariables</c> of its launch profile, and environment variables named
    /// <c>ISOPLANE_APP_&lt;Key&gt;</c>; the application sees none of the machine's other environment
    /// variables.
    /// <see href="https://isoplane.dev/docs/local-settings">Supply local development settings</see>
    /// describes each source. The environment name comes from the supplied
    /// <c>ASPNETCORE_ENVIRONMENT</c> or <c>DOTNET_ENVIRONMENT</c> setting, and is <c>Production</c>
    /// without one. The <c>Logging</c> configuration section sets log levels.
    /// </para>
    /// <para>
    /// Isoplane runs <c>Program.cs</c> during template preparation, up to <c>RunAsync</c>, and saves the
    /// result as the template every instance starts from. The supplied settings are available while
    /// it runs, and they are the settings every instance has: when one changes, the application is
    /// prepared again with the new value. <c>Program.cs</c> can therefore read a connection string
    /// and use it, for example to warm a route that queries a database.
    /// </para>
    /// <para>
    /// Some tools run <c>Program.cs</c> on your own machine rather than under Isoplane:
    /// <c>dotnet ef</c>, for one, and an in-process test host. The builder then behaves as
    /// <c>WebApplication.CreateBuilder</c> does. It reads the <c>appsettings</c> files, User Secrets
    /// in the Development environment and the machine's environment variables, and logs to the
    /// console.
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

    /// <summary>
    /// Sends the application one <c>GET</c> request from within, so that the work a route's first
    /// request does is already done when real requests arrive.
    /// </summary>
    /// <param name="application">The application built in <c>Program.cs</c>.</param>
    /// <param name="path">The path to request, beginning with a slash, with an optional query string: <c>/todos?page=1</c>.</param>
    /// <remarks>
    /// <para>
    /// The first request to a route is slower than those that follow: ASP.NET builds the route's
    /// handler, serializers are created, a database model is compiled. Isoplane saves the
    /// application's memory as a template after it has started, and every instance starts from that
    /// template. A request served before the template is saved leaves all of that work in it, so no
    /// instance repeats it.
    /// </para>
    /// <para>
    /// Call it from a callback subscribed to the
    /// <see href="https://isoplane.dev/docs/reference/sdk/Isoplane.Sdk/Isoplane/Initialization/Initializing">Initialization.Initializing</see>
    /// event, which is raised when the application has started and the template has not yet been
    /// saved. Call it once for each route worth warming.
    /// </para>
    /// <para>
    /// The request is a real one. It passes through the application's middleware and runs the
    /// endpoint, including any database query the endpoint makes, so choose routes that only read.
    /// Its <c>Host</c> header is <c>localhost</c>, and its response body is discarded. The endpoint
    /// must answer with a success status; any other status throws, which stops the application from
    /// starting.
    /// </para>
    /// </remarks>
    /// <example>
    /// <code>
    /// var app = builder.Build();
    /// app.MapGet("/todos", (TodoStore store) => store.All());
    ///
    /// Initialization.Initializing += async () =>
    /// {
    ///     await app.WarmupAsync("/todos");
    /// };
    ///
    /// await app.RunAsync();
    /// </code>
    /// </example>
    /// <exception cref="ArgumentException">The path is empty, or does not begin with a single slash.</exception>
    /// <exception cref="InvalidOperationException">
    /// The application has not started, because the call was made outside an
    /// <c>Initializing</c> callback, or it was not created with <see cref="CreateBuilder(string[])"/>.
    /// </exception>
    /// <exception cref="HttpRequestException">The endpoint answered with a status outside 200–299.</exception>
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
    internal static IWebHostBuilder UseIsoplane(this IWebHostBuilder builder)
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
            // IHttpClientFactory retires a handler on a timer it starts when it creates a client.
            // A timer started during initialization cannot be captured, so a route that uses a
            // client could not be warmed up; and an instance does not outlive a handler's
            // connections long enough for retiring them to matter.
            services.ConfigureHttpClientDefaults(client => client.SetHandlerLifetime(Timeout.InfiniteTimeSpan));
        });
    }
}
