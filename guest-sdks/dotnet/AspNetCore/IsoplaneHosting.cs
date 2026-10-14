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
    /// <c>ILogger</c> output goes to the application's log, which local development shows in the
    /// terminal running the application. The console logger works as written: after
    /// <c>builder.Logging.AddConsole()</c>, <c>AddJsonConsole</c>, <c>AddSimpleConsole</c> or
    /// <c>AddSystemdConsole</c>, each entry reaches the log as the chosen formatter writes it, in
    /// place of Isoplane's own record, and an entry the console logger sends to standard error is
    /// recorded at the log's error level. <c>ClearProviders</c> removes Isoplane's provider with the
    /// others, so an application that adds none after it logs nothing.
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
        var logs = new IsoplaneLoggerProvider();
        builder.Logging.AddProvider(logs);
        // The application adds its own providers after this returns, so the console logger is
        // replaced once all of them are registered.
        builder.Host.ConfigureContainer<object>((_, container) => ConsoleLogging.UseSynchronousConsole(container, logs));
        builder.Services.AddRouting();
        // Outside the sandbox a synchronous load can succeed, so only here does a failed one mean UseLoadedSession is missing.
        builder.Services.CheckSessionLoading();
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
    /// <para>
    /// To warm a route that takes another method, a body or particular headers, pass an
    /// <see cref="HttpRequestMessage"/> to <see cref="WarmupAsync(WebApplication, HttpRequestMessage)"/>.
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
    public static Task WarmupAsync(this WebApplication application, string path) =>
        application.WarmupAsync(HttpMethod.Get.Method, path, [], [], nameof(path));

    /// <summary>
    /// Sends the application one request of any method from within, with the headers and body
    /// given, so that the work a route's first request does is already done when real requests
    /// arrive.
    /// </summary>
    /// <param name="application">The application built in <c>Program.cs</c>.</param>
    /// <param name="request">
    /// The request to serve. Its address is a path beginning with a slash, with an optional query
    /// string. The caller keeps it and disposes it.
    /// </param>
    /// <remarks>
    /// <para>
    /// Use it for a route that <see cref="WarmupAsync(WebApplication, string)"/> cannot reach with a
    /// plain <c>GET</c>: a search or a query sent by <c>POST</c>, or an endpoint that answers only
    /// when a header is present. Everything that method's remarks say applies here: call it from an
    /// <c>Initialization.Initializing</c> callback, the endpoint must answer with a success status,
    /// and the response body is discarded.
    /// </para>
    /// <para>
    /// The request is real and can repeat if preparation retries initialization. Reusing an
    /// existing prepared template sends no request. Send one that changes nothing, or that is
    /// harmless to repeat: retries do not undo a <c>POST</c> that already created a row.
    /// </para>
    /// <para>
    /// The request carries the headers of the message and of its content. A <c>Host</c> header of
    /// <c>localhost</c> is added when the message names none.
    /// </para>
    /// </remarks>
    /// <example>
    /// <code>
    /// app.MapPost("/search", (SearchQuery query, Catalog catalog) => catalog.Search(query));
    ///
    /// Initialization.Initializing += async () =>
    /// {
    ///     using var search = new HttpRequestMessage(HttpMethod.Post, "/search")
    ///     {
    ///         Content = JsonContent.Create(new SearchQuery("warm-up"))
    ///     };
    ///     await app.WarmupAsync(search);
    /// };
    /// </code>
    /// </example>
    /// <exception cref="ArgumentException">The request's address is missing, names a host, or does not begin with a single slash.</exception>
    /// <exception cref="InvalidOperationException">
    /// The application has not started, because the call was made outside an
    /// <c>Initializing</c> callback, or it was not created with <see cref="CreateBuilder(string[])"/>.
    /// </exception>
    /// <exception cref="HttpRequestException">The endpoint answered with a status outside 200–299.</exception>
    public static async Task WarmupAsync(this WebApplication application, HttpRequestMessage request)
    {
        ArgumentNullException.ThrowIfNull(request);
        var body = request.Content is null ? [] : await request.Content.ReadAsByteArrayAsync();
        var headers = request.Headers.Concat(request.Content?.Headers ?? Enumerable.Empty<KeyValuePair<string, IEnumerable<string>>>())
            // The length is that of the bytes just read, whether or not the content had computed one.
            .Where(header => !header.Key.Equals("Content-Length", StringComparison.OrdinalIgnoreCase))
            .SelectMany(header => header.Value.Select(value => new KeyValuePair<string, string>(header.Key, value)))
            .ToList();
        if (request.Content is not null) headers.Add(new("Content-Length", body.Length.ToString(System.Globalization.CultureInfo.InvariantCulture)));
        var path = request.RequestUri is { IsAbsoluteUri: false } address ? address.OriginalString : "";
        await application.WarmupAsync(request.Method.Method, path, headers, body, nameof(request));
    }

    private static async Task WarmupAsync(this WebApplication application, string method, string path,
        IReadOnlyList<KeyValuePair<string, string>> headers, byte[] body, string parameter)
    {
        var server = application.Services.GetRequiredService<IServer>() as IsoplaneServer
            ?? throw new InvalidOperationException("Create the application with IsoplaneHosting.CreateBuilder.");
        if (!server.Started)
            throw new InvalidOperationException("WarmupAsync needs the started application. Call it from an Isoplane.Initialization.Initializing callback, which runs once RunAsync has started the host.");
        if (string.IsNullOrEmpty(path) || !path.StartsWith('/') || path.StartsWith("//", StringComparison.Ordinal))
            throw new ArgumentException("Use an application-relative path beginning with a slash.", parameter);
        var query = path.IndexOf('?');
        // Every real request names a host, and code that builds absolute URLs,
        // such as Blazor's navigation, fails without one.
        var named = headers.Any(header => header.Key.Equals("Host", StringComparison.OrdinalIgnoreCase));
        var request = Request.FromDispatch(new Isoplane.Abi.DispatchRecord
        {
            Kind = Isoplane.Abi.DispatchKind.Http,
            Method = method,
            Path = query < 0 ? path : path[..query],
            Url = path,
            Headers = named ? [.. headers] : [new("Host", "localhost"), .. headers],
            Body = body
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
            // IHttpClientFactory retires a handler on a timer it starts when it creates a client,
            // and a handler that keeps idle connections scavenges them on a timer it starts with
            // its first one. A timer cannot be captured in a template, and one still pending when
            // a request ends costs the request its instance. A connection lifetime of zero keeps
            // no connection and starts no timer: each call closes its connection once the
            // response is read, so none outlives a DNS change or goes stale between requests. An
            // idle timeout of zero would also start no timer, but it leaves each connection open
            // in a pool nothing visits again.
            services.ConfigureHttpClientDefaults(client => client
                .SetHandlerLifetime(Timeout.InfiniteTimeSpan)
                .ConfigurePrimaryHttpMessageHandler((handler, _) =>
                {
                    if (handler is SocketsHttpHandler sockets) sockets.PooledConnectionLifetime = TimeSpan.Zero;
                }));
        });
    }
}
