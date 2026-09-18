using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using Microsoft.Extensions.Configuration;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Hosting;
using Microsoft.Extensions.Logging;
using Microsoft.Extensions.Options;

namespace Acceptance;

/// <summary>
/// The Microsoft.Extensions stack a customer's application is built on:
/// dependency injection with its scope guards, configuration from memory
/// and JSON (and its file providers on the empty disk), options, logging
/// into a sink the route owns, activities and diagnostic listeners, the
/// generic host's lifecycle, and the process facts the runtime reports.
/// </summary>
internal sealed partial class Hosting
{
    public async Task<string> RunAsync(string[] path)
    {
        switch (path[0])
        {
            case "di":
                return DependencyInjection();
            case "di-guards":
                return DependencyInjectionGuards();
            case "config-memory":
                return ConfigurationFromMemory();
            case "config-json":
                return ConfigurationFromJson();
            case "config-files":
                return ConfigurationFromFiles();
            case "options":
                return Options();
            case "logging":
                return Logging();
            case "activity":
                return Activities();
            case "diagnostic-listener":
                return DiagnosticListeners();
            case "host":
                return await GenericHost(defaults: false).ConfigureAwait(false);
            case "host-defaults":
                return await GenericHost(defaults: true).ConfigureAwait(false);
            case "stopwatch":
                return StopwatchFacts();
            case "environment":
                return EnvironmentFacts();
            default:
                throw new ArgumentException("unknown hosting case " + path[0]);
        }
    }

    private interface IClock
    {
        int Tick();
    }

    private sealed class CountingClock : IClock
    {
        private int _ticks;

        public int Tick() => ++_ticks;
    }

    private interface IGreeter
    {
        string Greet();
    }

    private sealed class Greeter(IClock clock) : IGreeter
    {
        public string Greet() => "tick " + clock.Tick().ToString(CultureInfo.InvariantCulture);
    }

    private interface IRepository<T>
    {
        string Describe();
    }

    private sealed class Repository<T> : IRepository<T>
    {
        public string Describe() => typeof(T).Name;
    }

    private sealed class Needy(IGreeter greeter, IRepository<string> strings, IEnumerable<IClock> clocks)
    {
        public string Describe() => greeter.Greet() + " " + strings.Describe() + " clocks=" + clocks.Count().ToString(CultureInfo.InvariantCulture);
    }

    private sealed class NeedsMissing(Stream missing)
    {
        public long Length => missing.Length;
    }

    private static string DependencyInjection()
    {
        var services = new ServiceCollection();
        services.AddSingleton<IClock, CountingClock>();
        services.AddScoped<IGreeter, Greeter>();
        services.AddTransient<Needy>();
        services.AddTransient(typeof(IRepository<>), typeof(Repository<>));
        using ServiceProvider provider = services.BuildServiceProvider();
        string first;
        string second;
        IGreeter greeterOfFirstScope;
        using (IServiceScope scope = provider.CreateScope())
        {
            greeterOfFirstScope = scope.ServiceProvider.GetRequiredService<IGreeter>();
            first = scope.ServiceProvider.GetRequiredService<Needy>().Describe()
                + " same-greeter=" + ReferenceEquals(greeterOfFirstScope, scope.ServiceProvider.GetRequiredService<IGreeter>());
        }
        using (IServiceScope scope = provider.CreateScope())
        {
            second = scope.ServiceProvider.GetRequiredService<Needy>().Describe()
                + " new-greeter=" + !ReferenceEquals(greeterOfFirstScope, scope.ServiceProvider.GetRequiredService<IGreeter>());
        }
        // The open generic closed over a value type is an instantiation the
        // ahead-of-time compiler never saw.
        return first + " | " + second + " | ints=" + Attempt(() => provider.GetRequiredService<IRepository<int>>().Describe());
    }

    private static string DependencyInjectionGuards()
    {
        var services = new ServiceCollection();
        services.AddScoped<IGreeter, Greeter>();
        services.AddSingleton<IClock, CountingClock>();
        var parts = new List<string>();
        using (ServiceProvider provider = services.BuildServiceProvider(new ServiceProviderOptions { ValidateScopes = true }))
        {
            parts.Add("scoped-from-root=" + Attempt(() => provider.GetRequiredService<IGreeter>().Greet()));
            parts.Add("unregistered=" + Attempt(() => provider.GetRequiredService<Needy>().Describe()));
            parts.Add("optional=" + (provider.GetService<Needy>() is null ? "null" : "instance"));
        }
        services.AddTransient<NeedsMissing>();
        parts.Add("validate-on-build=" + Attempt(() =>
        {
            using ServiceProvider provider = services.BuildServiceProvider(new ServiceProviderOptions { ValidateOnBuild = true });
            return "built";
        }));
        return string.Join(" ", parts);
    }

    /// <summary>
    /// An options type the binder's source generator cannot reach (private
    /// and nested), so binding it falls back to reflection over whatever
    /// the trimmer kept; nothing in the guest assigns these properties.
    /// </summary>
    private sealed class HiddenOptions
    {
        public bool Enabled { get; set; }

        public int Level { get; set; }
    }

    private static string ConfigurationFromMemory()
    {
        IConfigurationRoot configuration = new ConfigurationBuilder()
            .AddInMemoryCollection(new Dictionary<string, string?>
            {
                ["Server:Name"] = "edge",
                ["Server:Port"] = "8443",
                ["Server:Tls"] = "true",
                ["Server:Hosts:0"] = "a.example",
                ["Server:Hosts:1"] = "b.example",
                ["Feature"] = "on",
                ["Hidden:Enabled"] = "true",
                ["Hidden:Level"] = "3",
            })
            .AddInMemoryCollection(new Dictionary<string, string?> { ["Server:Port"] = "9443" })
            .Build();
        var options = new ServerOptions();
        configuration.GetSection("Server").Bind(options);
        var hidden = new HiddenOptions();
        configuration.GetSection("Hidden").Bind(hidden);
        return string.Join("|",
            configuration["Feature"] ?? "null",
            configuration.GetValue<int>("Server:Port").ToString(CultureInfo.InvariantCulture),
            configuration.GetValue("Missing", 7).ToString(CultureInfo.InvariantCulture),
            (configuration["Server:Tls"] ?? "null") + "/" + configuration.GetValue<bool>("Server:Tls"),
            options.Name + ":" + options.Port.ToString(CultureInfo.InvariantCulture) + ":" + options.Tls + ":" + string.Join(",", options.Hosts),
            string.Join(",", configuration.GetSection("Server").GetChildren().Select(child => child.Key)),
            configuration.GetSection("Nowhere").Exists().ToString(),
            "hidden=" + hidden.Enabled + "/" + hidden.Level.ToString(CultureInfo.InvariantCulture));
    }

    private static string ConfigurationFromJson()
    {
        const string json = """
            {
              "Server": { "Name": "json-edge", "Port": 8080, "Tls": false, "Hosts": ["j.example"] },
              "Nested": { "Deep": { "Key": "value" } },
              "Array": [ "x", "y", "z" ]
            }
            """;
        IConfigurationRoot configuration = new ConfigurationBuilder()
            .AddJsonStream(new MemoryStream(Encoding.UTF8.GetBytes(json)))
            .Build();
        ServerOptions options = configuration.GetSection("Server").Get<ServerOptions>() ?? new ServerOptions();
        return string.Join("|",
            options.Name + ":" + options.Port.ToString(CultureInfo.InvariantCulture) + ":" + options.Tls + ":" + string.Join(",", options.Hosts),
            configuration["Nested:Deep:Key"] ?? "null",
            configuration["Array:2"] ?? "null",
            configuration.GetSection("Array").GetChildren().Count().ToString(CultureInfo.InvariantCulture));
    }

    /// <summary>File-backed providers on the empty read-only disk.</summary>
    private static string ConfigurationFromFiles()
    {
        return string.Join(" ",
            "required=" + Attempt(() => new ConfigurationBuilder().AddJsonFile("/appsettings.json", optional: false).Build()["Any"] ?? "built"),
            "optional=" + Attempt(() => new ConfigurationBuilder().AddJsonFile("/appsettings.json", optional: true).Build()["Any"] ?? "built"),
            "relative-optional=" + Attempt(() => new ConfigurationBuilder().AddJsonFile("appsettings.json", optional: true).Build()["Any"] ?? "built"),
            "environment=" + new ConfigurationBuilder().AddEnvironmentVariables().Build().AsEnumerable().Count().ToString(CultureInfo.InvariantCulture),
            "base-directory=" + AppContext.BaseDirectory);
    }

    private static string Options()
    {
        IConfigurationRoot configuration = new ConfigurationBuilder()
            .AddInMemoryCollection(new Dictionary<string, string?> { ["Server:Name"] = "opt", ["Server:Port"] = "1" })
            .Build();
        var services = new ServiceCollection();
        services.Configure<ServerOptions>(configuration.GetSection("Server"));
        services.PostConfigure<ServerOptions>(options => options.Port += 10);
        services.Configure<ServerOptions>("named", options => options.Name = "named");
        using ServiceProvider provider = services.BuildServiceProvider();
        ServerOptions plain = provider.GetRequiredService<IOptions<ServerOptions>>().Value;
        ServerOptions monitored = provider.GetRequiredService<IOptionsMonitor<ServerOptions>>().Get("named");
        using IServiceScope scope = provider.CreateScope();
        ServerOptions snapshot = scope.ServiceProvider.GetRequiredService<IOptionsSnapshot<ServerOptions>>().Value;
        return plain.Name + ":" + plain.Port.ToString(CultureInfo.InvariantCulture)
            + "|" + monitored.Name + ":" + monitored.Port.ToString(CultureInfo.InvariantCulture)
            + "|" + snapshot.Name + ":" + snapshot.Port.ToString(CultureInfo.InvariantCulture)
            + "|same-singleton=" + ReferenceEquals(plain, provider.GetRequiredService<IOptions<ServerOptions>>().Value);
    }

    private sealed class CapturingProvider : ILoggerProvider, ISupportExternalScope
    {
        private IExternalScopeProvider? _scopes;

        public List<string> Lines { get; } = new();

        public ILogger CreateLogger(string categoryName) => new CapturingLogger(this, categoryName);

        public void SetScopeProvider(IExternalScopeProvider scopeProvider) => _scopes = scopeProvider;

        public void Dispose()
        {
        }

        private sealed class CapturingLogger(CapturingProvider owner, string category) : ILogger
        {
            public IDisposable? BeginScope<TState>(TState state) where TState : notnull => owner._scopes?.Push(state);

            public bool IsEnabled(LogLevel logLevel) => true;

            public void Log<TState>(LogLevel logLevel, EventId eventId, TState state, Exception? exception, Func<TState, Exception?, string> formatter)
            {
                var scopes = new List<string>();
                owner._scopes?.ForEachScope((scope, list) => list.Add(scope?.ToString() ?? "null"), scopes);
                string fields = state is IReadOnlyList<KeyValuePair<string, object?>> pairs
                    ? string.Join(",", pairs.Where(pair => pair.Key != "{OriginalFormat}").Select(pair => pair.Key + "=" + pair.Value))
                    : "";
                owner.Lines.Add(logLevel + " " + category + " " + eventId.Id.ToString(CultureInfo.InvariantCulture) + " " + formatter(state, exception)
                    + " [" + fields + "] {" + string.Join(";", scopes) + "}" + (exception is null ? "" : " " + exception.GetType().Name));
            }
        }
    }

    private static partial class Messages
    {
        [LoggerMessage(EventId = 7, Level = LogLevel.Warning, Message = "budget {Budget} exceeded by {Instance}")]
        public static partial void BudgetExceeded(ILogger logger, int budget, string instance);
    }

    private static string Logging()
    {
        var sink = new CapturingProvider();
        using ILoggerFactory factory = LoggerFactory.Create(builder =>
        {
            builder.AddProvider(sink);
            builder.SetMinimumLevel(LogLevel.Debug);
            builder.AddFilter("Quiet", LogLevel.Warning);
        });
        ILogger<Hosting> logger = factory.CreateLogger<Hosting>();
        ILogger quiet = factory.CreateLogger("Quiet");
        using (logger.BeginScope("request {Id}", 42))
        {
            logger.LogInformation("handled {Path} in {Ms} ms", "/x", 3);
            logger.LogTrace("dropped by the minimum level");
            Messages.BudgetExceeded(logger, 50, "inst-1");
        }
        quiet.LogInformation("dropped by the category filter");
        quiet.LogError(new InvalidOperationException("boom"), "kept {Why}", "error");
        return string.Join("\n", sink.Lines);
    }

    private static string Activities()
    {
        using var source = new ActivitySource("Isoplane.Acceptance");
        var started = new List<string>();
        using var listener = new ActivityListener
        {
            ShouldListenTo = candidate => candidate.Name == "Isoplane.Acceptance",
            Sample = (ref ActivityCreationOptions<ActivityContext> _) => ActivitySamplingResult.AllDataAndRecorded,
            ActivityStarted = activity => started.Add(activity.OperationName),
        };
        ActivitySource.AddActivityListener(listener);
        string parentId;
        string childParent;
        bool childSharesTrace;
        bool currentRestored;
        using (Activity? parent = source.StartActivity("dispatch"))
        {
            if (parent is null) return "no parent activity";
            parent.SetTag("isoplane.instance", 7);
            parentId = parent.Id ?? "null";
            using (Activity? child = source.StartActivity("stage", ActivityKind.Internal))
            {
                if (child is null) return "no child activity";
                childParent = child.ParentSpanId.ToHexString() == parent.SpanId.ToHexString() ? "parent" : "other";
                childSharesTrace = child.TraceId == parent.TraceId;
            }
            currentRestored = ReferenceEquals(Activity.Current, parent);
        }
        Activity? none = new ActivitySource("Unlistened").StartActivity("ignored");
        return string.Join("|",
            "started=" + string.Join(",", started),
            "id-format=" + (parentId.StartsWith("00-", StringComparison.Ordinal) && parentId.Length == 55 ? "w3c" : parentId),
            "child-parent=" + childParent,
            "shared-trace=" + childSharesTrace,
            "current-restored=" + currentRestored,
            "after=" + (Activity.Current is null ? "none" : "leaked"),
            "unlistened=" + (none is null ? "null" : "created"));
    }

    private sealed class Observer : IObserver<KeyValuePair<string, object?>>
    {
        public List<string> Seen { get; } = new();

        public void OnCompleted()
        {
        }

        public void OnError(Exception error)
        {
        }

        public void OnNext(KeyValuePair<string, object?> value) => Seen.Add(value.Key + "=" + (value.Value?.ToString() ?? "null"));
    }

    private static string DiagnosticListeners()
    {
        using var listener = new DiagnosticListener("Isoplane.Acceptance.Diag");
        var observer = new Observer();
        using (listener.Subscribe(observer, name => name.StartsWith("Kept", StringComparison.Ordinal)))
        {
            if (listener.IsEnabled("Kept.Event")) listener.Write("Kept.Event", "payload");
            if (listener.IsEnabled("Dropped.Event")) listener.Write("Dropped.Event", "payload");
        }
        listener.Write("Kept.AfterUnsubscribe", "payload");
        return "seen=" + string.Join(",", observer.Seen) + " enabled-after=" + listener.IsEnabled("Kept.Event");
    }

    private sealed class Recorder
    {
        public List<string> Events { get; } = new();
    }

    private sealed class RecordingService(Recorder recorder, ILogger<RecordingService> logger) : IHostedService
    {
        public Task StartAsync(CancellationToken cancellationToken)
        {
            recorder.Events.Add("started");
            logger.LogInformation("service started");
            return Task.CompletedTask;
        }

        public Task StopAsync(CancellationToken cancellationToken)
        {
            recorder.Events.Add("stopped");
            return Task.CompletedTask;
        }
    }

    private static async Task<string> GenericHost(bool defaults)
    {
        try
        {
            HostApplicationBuilder builder = defaults
                ? Host.CreateApplicationBuilder()
                : Host.CreateEmptyApplicationBuilder(new HostApplicationBuilderSettings { ApplicationName = "acceptance" });
            builder.Configuration.AddInMemoryCollection(new Dictionary<string, string?> { ["Greeting"] = "hi" });
            builder.Services.AddSingleton<Recorder>();
            builder.Services.AddHostedService<RecordingService>();
            using IHost host = builder.Build();
            var recorder = host.Services.GetRequiredService<Recorder>();
            await host.StartAsync().ConfigureAwait(false);
            recorder.Events.Add("running");
            await host.StopAsync().ConfigureAwait(false);
            IHostEnvironment environment = host.Services.GetRequiredService<IHostEnvironment>();
            return string.Join(",", recorder.Events)
                + " greeting=" + (host.Services.GetRequiredService<IConfiguration>()["Greeting"] ?? "null")
                + " app=" + environment.ApplicationName
                + " env=" + environment.EnvironmentName
                + " root=" + Quote(environment.ContentRootPath);
        }
        catch (Exception error)
        {
            return error.GetType().Name + ": " + error.Message;
        }
    }

    private static string StopwatchFacts()
    {
        long before = Stopwatch.GetTimestamp();
        var watch = Stopwatch.StartNew();
        watch.Stop();
        long after = Stopwatch.GetTimestamp();
        return "high-resolution=" + Stopwatch.IsHighResolution
            + " frequency=" + Stopwatch.Frequency.ToString(CultureInfo.InvariantCulture)
            + " monotonic=" + (after >= before)
            + " elapsed-non-negative=" + (watch.ElapsedTicks >= 0)
            + " tick-count-positive=" + (Environment.TickCount64 > 0);
    }

    private static string EnvironmentFacts()
    {
        return string.Join("|",
            "cpus=" + Environment.ProcessorCount.ToString(CultureInfo.InvariantCulture),
            "64bit=" + Environment.Is64BitProcess,
            "platform=" + Environment.OSVersion.Platform,
            "arch=" + RuntimeInformation.ProcessArchitecture,
            "rid=" + RuntimeInformation.RuntimeIdentifier,
            "os=" + RuntimeInformation.OSDescription,
            "framework=" + RuntimeInformation.FrameworkDescription,
            "version=" + Environment.Version.Major.ToString(CultureInfo.InvariantCulture),
            "dynamic-code=" + System.Runtime.CompilerServices.RuntimeFeature.IsDynamicCodeSupported,
            "variables=" + Environment.GetEnvironmentVariables().Count.ToString(CultureInfo.InvariantCulture),
            "args=" + Environment.GetCommandLineArgs().Length.ToString(CultureInfo.InvariantCulture),
            "cwd=" + Attempt(() => Environment.CurrentDirectory),
            "machine=" + Attempt(() => Environment.MachineName),
            "user=" + Attempt(() => Environment.UserName),
            "threads=" + Attempt(() => Environment.CurrentManagedThreadId.ToString(CultureInfo.InvariantCulture)),
            "pid=" + Attempt(() => Environment.ProcessId.ToString(CultureInfo.InvariantCulture)));
    }

    private static string Attempt(Func<string> operation)
    {
        try
        {
            return operation();
        }
        catch (Exception error)
        {
            return error.GetType().Name;
        }
    }

    private static string Quote(string text) => "'" + text + "'";
}

/// <summary>
/// An options type the configuration binder's source generator can reach,
/// so binding it is generated code rather than reflection.
/// </summary>
internal sealed class ServerOptions
{
    /// <summary>The server's name.</summary>
    public string Name { get; set; } = "";

    /// <summary>The port to listen on.</summary>
    public int Port { get; set; }

    /// <summary>Whether TLS is on.</summary>
    public bool Tls { get; set; }

    /// <summary>The host names served.</summary>
    public List<string> Hosts { get; set; } = new();
}
